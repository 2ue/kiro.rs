use super::*;
use futures::StreamExt;
use std::sync::Arc;

const SONNET_MODEL: &str = "claude-sonnet-4.5";

#[test]
fn runtime_state_apply_rejects_stale_and_equal_revisions() {
    let manager = MultiTokenManager::new(
        Config::default(),
        vec![test_access_token_credential("runtime-revision", "Pro")],
        None,
        None,
        false,
    )
    .unwrap();
    let mut entries = manager.entries.lock();
    let entry = &mut entries[0];
    entry.failure_count = 2;
    entry.runtime_revision = 5;

    for revision in [4, 5] {
        let stale = CredentialRuntimeStateRow {
            failure_count: 0,
            revision,
            ..CredentialRuntimeStateRow::default()
        };
        assert!(!MultiTokenManager::apply_runtime_state_if_newer(
            entry, &stale
        ));
        assert_eq!(entry.failure_count, 2);
        assert_eq!(entry.runtime_revision, 5);
    }

    let newer = CredentialRuntimeStateRow {
        failure_count: 3,
        disabled_reason: Some(DisabledReason::TooManyFailures.as_str().to_string()),
        revision: 6,
        ..CredentialRuntimeStateRow::default()
    };
    assert!(MultiTokenManager::apply_runtime_state_if_newer(
        entry, &newer
    ));
    assert_eq!(entry.failure_count, 3);
    assert_eq!(entry.runtime_revision, 6);
    assert!(entry.disabled);
    assert_eq!(entry.disabled_reason, Some(DisabledReason::TooManyFailures));
}

#[test]
fn atomic_credential_runtime_reconcile_requires_the_complete_patch() {
    let base = KiroCredentials {
        id: Some(7),
        email: Some("old@example.com".to_string()),
        disabled: true,
        ..Default::default()
    };
    let mut requested = base.clone();
    requested.email = Some("new@example.com".to_string());
    let mut current = requested.clone();
    current.disabled = false;
    current.storage_revision = 2;
    let runtime = CredentialRuntimeStateRow {
        failure_count: 0,
        refresh_failure_count: 0,
        disabled_reason: None,
        warmup_remaining: 3,
        generation: 0,
        revision: 2,
    };
    let patch = CredentialRuntimeStatePatch {
        failure_count: Some(0),
        refresh_failure_count: Some(0),
        disabled_reason: CredentialRuntimeDisabledReasonPatch::Clear,
        warmup_remaining: Some(3),
        credential_disabled: Some(false),
        ..Default::default()
    };

    assert!(
        MultiTokenManager::credential_runtime_patch_is_applied(
            &base,
            &requested,
            &current,
            Some(&runtime),
            &patch,
        )
        .unwrap()
    );

    let mut incomplete_runtime = runtime.clone();
    incomplete_runtime.refresh_failure_count = 1;
    assert!(
        !MultiTokenManager::credential_runtime_patch_is_applied(
            &base,
            &requested,
            &current,
            Some(&incomplete_runtime),
            &patch,
        )
        .unwrap()
    );
    assert!(
        !MultiTokenManager::credential_runtime_patch_is_applied(
            &base, &requested, &current, None, &patch,
        )
        .unwrap()
    );

    let mut unverifiable_patch = patch;
    unverifiable_patch.last_used_at = Some("2026-07-10T08:00:00Z".to_string());
    assert!(
        !MultiTokenManager::credential_runtime_patch_is_applied(
            &base,
            &requested,
            &current,
            Some(&runtime),
            &unverifiable_patch,
        )
        .unwrap()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn credential_pgsql_sync_bridge_enforces_deadline() {
    let started_at = Instant::now();
    let error = block_on_storage("test PgSQL bridge", async {
        credential_pgsql_sync_with_timeout(
            "test PgSQL operation",
            StdDuration::from_millis(20),
            std::future::pending::<anyhow::Result<()>>(),
        )
        .await
    })
    .unwrap_err();

    assert!(error.to_string().contains("test PgSQL operation"));
    assert!(error.to_string().contains("20ms"));
    assert!(started_at.elapsed() < StdDuration::from_millis(500));

    let value = block_on_storage("successful PgSQL bridge", async {
        credential_pgsql_sync_with_timeout(
            "successful PgSQL operation",
            StdDuration::from_millis(20),
            async { Ok::<_, anyhow::Error>(42) },
        )
        .await
    })
    .unwrap();
    assert_eq!(value, 42);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stats_flush_worker_shutdown_flushes_and_releases_manager() {
    let manager = Arc::new(
        MultiTokenManager::new(
            Config::default(),
            vec![api_key_credential("stats-worker-test")],
            None,
            None,
            false,
        )
        .unwrap(),
    );
    let worker = manager.spawn_stats_flush_worker();

    let report = worker.shutdown(StdDuration::from_secs(1)).await;

    assert!(report.signal_sent);
    assert!(report.flushed);
    assert!(!report.timed_out);
    assert!(!report.task_failed);
    assert_eq!(report.pending_runtime_mutations, 0);
    assert_eq!(report.overflow_runtime_mutations, 0);
    assert_eq!(Arc::strong_count(&manager), 1);
}

async fn test_redis_store() -> Option<Arc<RedisStore>> {
    let url = crate::storage::integration_test_url("KIRO_RS_TEST_REDIS_URL")?;
    let mut config = Config::default();
    config.redis.url = Some(url);
    config.redis.key_prefix = format!("kiro_rs:test:{}", uuid::Uuid::new_v4());
    Some(Arc::new(RedisStore::connect(&config).await.unwrap()))
}

async fn occupy_redis_dispatch_slots(
    redis: &RedisStore,
    credential_ids: &[u64],
    credential_max: u32,
    global_max: u32,
    lease_base: u64,
) -> Vec<(u64, u64)> {
    let mut leases = Vec::with_capacity(credential_ids.len());
    for (offset, credential_id) in credential_ids.iter().copied().enumerate() {
        let lease_id = lease_base + offset as u64;
        let acquired = redis
            .acquire_dispatch_lease(
                credential_id,
                lease_id,
                credential_max,
                global_max,
                1,
                Some(StdDuration::from_secs(60)),
                InFlightKind::Api.as_str(),
            )
            .await
            .unwrap();
        assert!(
            acquired.is_some(),
            "failed to prefill credential {credential_id}"
        );
        leases.push((credential_id, lease_id));
    }
    leases
}

async fn release_redis_dispatch_slots(redis: &RedisStore, leases: &[(u64, u64)]) {
    for (credential_id, lease_id) in leases.iter().copied() {
        redis
            .release_in_flight_lease(credential_id, lease_id)
            .await
            .unwrap();
    }
}

async fn acquire_test_refresh_lock_until(
    redis: &RedisStore,
    credential_id: u64,
    deadline: tokio::time::Instant,
) -> String {
    loop {
        match tokio::time::timeout(
            StdDuration::from_millis(250),
            redis.acquire_refresh_lock(credential_id, 30),
        )
        .await
        {
            Ok(Ok(Some(lock_token))) => return lock_token,
            Ok(Ok(None)) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(StdDuration::from_millis(10)).await;
            }
            Ok(Ok(None)) => panic!("Redis Token 刷新锁未在期限内释放"),
            Ok(Err(error)) => panic!("重新获取 Redis Token 刷新锁失败: {error}"),
            Err(_) if tokio::time::Instant::now() < deadline => {}
            Err(_) => panic!("重新获取 Redis Token 刷新锁超过期限"),
        }
    }
}

async fn test_postgres_store() -> Option<Arc<PostgresStore>> {
    let url = crate::storage::integration_test_url("KIRO_RS_TEST_POSTGRES_URL")?;
    let mut config = Config::default();
    config.postgres.url = Some(url);
    config.postgres.max_connections = 2;
    Some(Arc::new(
        PostgresStore::connect_test(&config).await.unwrap(),
    ))
}

#[derive(Clone)]
struct ForceRefreshEndpointState {
    request_received: Arc<tokio::sync::Notify>,
}

async fn force_refresh_token_endpoint(
    axum::extract::State(state): axum::extract::State<ForceRefreshEndpointState>,
) -> axum::Json<serde_json::Value> {
    state.request_received.notify_one();
    axum::Json(serde_json::json!({
        "access_token": "force-refreshed-access-token",
        "refresh_token": "n".repeat(150),
        "expires_in": 3600,
        "scope": "offline_access codewhisperer:conversations",
    }))
}

async fn spawn_force_refresh_token_endpoint() -> (
    String,
    Arc<tokio::sync::Notify>,
    tokio::task::JoinHandle<()>,
) {
    let request_received = Arc::new(tokio::sync::Notify::new());
    let state = ForceRefreshEndpointState {
        request_received: request_received.clone(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = axum::Router::new()
        .route("/token", axum::routing::post(force_refresh_token_endpoint))
        .with_state(state);
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}/token"), request_received, server)
}

async fn pending_refresh_token_endpoint(
    axum::extract::State(request_received): axum::extract::State<Arc<tokio::sync::Notify>>,
) -> axum::Json<serde_json::Value> {
    request_received.notify_one();
    std::future::pending().await
}

async fn spawn_pending_refresh_token_endpoint() -> (
    String,
    Arc<tokio::sync::Notify>,
    tokio::task::JoinHandle<()>,
) {
    let request_received = Arc::new(tokio::sync::Notify::new());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = axum::Router::new()
        .route(
            "/token",
            axum::routing::post(pending_refresh_token_endpoint),
        )
        .with_state(request_received.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}/token"), request_received, server)
}

fn force_refresh_test_credential(token_endpoint: String) -> KiroCredentials {
    KiroCredentials {
        id: Some(1),
        auth_method: Some("external_idp".to_string()),
        access_token: Some("old-access-token".to_string()),
        refresh_token: Some("r".repeat(150)),
        client_id: Some("force-refresh-test-client".to_string()),
        token_endpoint: Some(token_endpoint),
        scopes: Some("offline_access codewhisperer:conversations".to_string()),
        expires_at: Some((Utc::now() + Duration::hours(1)).to_rfc3339()),
        ..Default::default()
    }
}

fn api_key_credential(token: &str) -> KiroCredentials {
    KiroCredentials {
        kiro_api_key: Some(token.to_string()),
        auth_method: Some("api_key".to_string()),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn postgres_stats_reload_preserves_pending_deltas_and_monotonic_local_values() {
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };
    let mut credential = api_key_credential("stats-reload-monotonic");
    credential.id = Some(1);
    store.save_credentials(&[credential.clone()]).await.unwrap();
    store
        .apply_credential_stats_deltas(
            uuid::Uuid::new_v4(),
            &HashMap::from([(
                1,
                CredentialStatsDeltaRow {
                    success_delta: 2,
                    selection_delta: 3,
                    last_used_at: Some("2024-01-01T00:00:00Z".to_string()),
                },
            )]),
        )
        .await
        .unwrap();

    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![credential],
        None,
        None,
        false,
        Some(store.clone()),
        None,
    )
    .unwrap();
    {
        let mut entries = manager.entries.lock();
        let entry = entries.iter_mut().find(|entry| entry.id == 1).unwrap();
        entry.success_count = 20;
        entry.total_selection_count = 30;
        entry.last_used_at = Some("2025-01-01T00:00:00Z".to_string());
    }
    manager.pending_stats_deltas.lock().insert(
        1,
        CredentialStatsDeltaRow {
            success_delta: 5,
            selection_delta: 7,
            last_used_at: Some("2026-01-01T00:00:00Z".to_string()),
        },
    );
    manager.mark_stats_dirty();

    manager.load_stats();
    manager.refresh_stats_from_postgres();

    {
        let entries = manager.entries.lock();
        let entry = entries.iter().find(|entry| entry.id == 1).unwrap();
        assert_eq!(entry.success_count, 20);
        assert_eq!(entry.total_selection_count, 30);
        assert_eq!(entry.last_used_at.as_deref(), Some("2025-01-01T00:00:00Z"));
    }
    {
        let pending = manager.pending_stats_deltas.lock();
        let delta = pending.get(&1).expect("reload must retain pending delta");
        assert_eq!(delta.success_delta, 5);
        assert_eq!(delta.selection_delta, 7);
        assert_eq!(delta.last_used_at.as_deref(), Some("2026-01-01T00:00:00Z"));
    }
    assert!(manager.pending_stats_batches.lock().is_empty());
    assert!(manager.stats_dirty.load(Ordering::Acquire));

    manager.pending_stats_deltas.lock().clear();
    assert!(manager.refresh_stats_dirty_from_pending());
    store.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn postgres_failed_stats_batch_retry_keeps_frozen_payload_and_new_accumulator_separate() {
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };
    let mut credential = api_key_credential("stats-frozen-batch");
    credential.id = Some(1);
    store.save_credentials(&[credential.clone()]).await.unwrap();
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![credential],
        None,
        None,
        false,
        Some(store.clone()),
        None,
    )
    .unwrap();
    manager.pending_stats_deltas.lock().insert(
        1,
        CredentialStatsDeltaRow {
            success_delta: 3,
            selection_delta: 4,
            last_used_at: Some("2025-01-01T00:00:00Z".to_string()),
        },
    );
    manager.mark_stats_dirty();

    // Removing this test's isolated schema makes the next writes fail immediately and
    // deterministically, without needing to model a commit-unknown network failure.
    store.drop_test_schema().await.unwrap();
    manager.save_stats();
    let first_attempt = manager
        .pending_stats_batches
        .lock()
        .front()
        .cloned()
        .expect("failed write must retain a frozen batch");
    let first_delta = first_attempt.deltas.get(&1).unwrap();
    assert_eq!(first_delta.success_delta, 3);
    assert_eq!(first_delta.selection_delta, 4);
    assert_eq!(
        first_delta.last_used_at.as_deref(),
        Some("2025-01-01T00:00:00Z")
    );

    manager.pending_stats_deltas.lock().insert(
        1,
        CredentialStatsDeltaRow {
            success_delta: 5,
            selection_delta: 6,
            last_used_at: Some("2026-01-01T00:00:00Z".to_string()),
        },
    );
    manager.mark_stats_dirty();
    manager.save_stats();

    {
        let batches = manager.pending_stats_batches.lock();
        assert_eq!(batches.len(), 1);
        let retried = batches.front().unwrap();
        assert_eq!(retried.operation_id, first_attempt.operation_id);
        assert_eq!(retried.deltas.len(), 1);
        let retried_delta = retried.deltas.get(&1).unwrap();
        assert_eq!(retried_delta.success_delta, 3);
        assert_eq!(retried_delta.selection_delta, 4);
        assert_eq!(
            retried_delta.last_used_at.as_deref(),
            Some("2025-01-01T00:00:00Z")
        );
    }
    {
        let pending = manager.pending_stats_deltas.lock();
        let new_delta = pending.get(&1).expect("new delta must remain mutable");
        assert_eq!(new_delta.success_delta, 5);
        assert_eq!(new_delta.selection_delta, 6);
        assert_eq!(
            new_delta.last_used_at.as_deref(),
            Some("2026-01-01T00:00:00Z")
        );
    }
    assert!(manager.stats_dirty.load(Ordering::Acquire));

    manager.pending_stats_batches.lock().clear();
    manager.pending_stats_deltas.lock().clear();
    assert!(manager.refresh_stats_dirty_from_pending());
}

#[test]
fn pending_success_replay_coalesces_without_quarantining_dispatch() {
    let manager = MultiTokenManager::new(
        Config::default(),
        vec![test_access_token_credential("pending-success", "Pro")],
        None,
        None,
        false,
    )
    .unwrap();
    let first_operation_id = uuid::Uuid::new_v4();
    let second_operation_id = uuid::Uuid::new_v4();
    let latest_operation_id = uuid::Uuid::new_v4();

    for operation_id in [first_operation_id, second_operation_id, latest_operation_id] {
        assert!(manager.enqueue_pending_runtime_mutation(
            1,
            PendingCredentialRuntimeMutation::Success {
                operation_id,
                expected_generation: 0,
                count: 1,
            },
        ));
    }

    {
        let pending = manager.pending_runtime_mutations.lock();
        let queue = pending.get(&1).expect("success replay queue");
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.front().unwrap().operation_id(), first_operation_id);
        match queue.back().unwrap() {
            PendingCredentialRuntimeMutation::Success {
                operation_id,
                expected_generation,
                count,
            } => {
                assert_eq!(*operation_id, latest_operation_id);
                assert_eq!(*expected_generation, 0);
                assert_eq!(*count, 2);
            }
            mutation => panic!("unexpected tail mutation: {mutation:?}"),
        }
    }
    {
        let entries = manager.entries.lock();
        assert!(!entries[0].runtime_persistence_degraded);
        assert!(!entries[0].disabled);
    }

    assert!(manager.enqueue_pending_runtime_mutation(
        1,
        PendingCredentialRuntimeMutation::ApiFailure {
            operation_id: uuid::Uuid::new_v4(),
            expected_generation: 0,
            last_used_at: Utc::now().to_rfc3339(),
        },
    ));
    let entries = manager.entries.lock();
    assert!(entries[0].runtime_persistence_degraded);
    assert!(entries[0].disabled);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn postgres_success_pool_saturation_stays_bounded_and_does_not_disable_credential() {
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };
    let mut credential = api_key_credential("success-pool-saturation");
    credential.id = Some(1);
    store.save_credentials(&[credential.clone()]).await.unwrap();
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![credential],
        None,
        None,
        false,
        Some(store.clone()),
        None,
    )
    .unwrap();
    manager.set_warmup_remaining(1, 5).unwrap();

    let first_connection = store.pool().acquire().await.unwrap();
    let second_connection = store.pool().acquire().await.unwrap();
    let started_at = Instant::now();
    for _ in 0..3 {
        manager.report_success(1);
    }
    let elapsed = started_at.elapsed();

    assert!(
        elapsed < StdDuration::from_millis(250),
        "success completion must not wait for a saturated PgSQL pool, elapsed={elapsed:?}"
    );
    assert_eq!(manager.runtime_mutation_backlog().0, 2);
    {
        let entries = manager.entries.lock();
        assert_eq!(entries[0].warmup_remaining, 2);
        assert!(!entries[0].runtime_persistence_degraded);
        assert!(!entries[0].disabled);
    }

    drop(first_connection);
    drop(second_connection);
    manager.flush_pending_runtime_mutations_with_budget(StdDuration::from_secs(2));

    assert_eq!(manager.runtime_mutation_backlog(), (0, 0));
    let runtime = store.load_credential_runtime_state().await.unwrap();
    assert_eq!(runtime[&1].warmup_remaining, 2);
    assert_eq!(runtime[&1].revision, 3);
    assert!(runtime[&1].disabled_reason.is_none());

    store.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn postgres_success_pool_saturation_keeps_40_credentials_dispatchable() {
    const CREDENTIAL_COUNT: u64 = 40;
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };
    let credentials: Vec<_> = (1..=CREDENTIAL_COUNT)
        .map(|id| {
            let mut credential = api_key_credential(&format!("saturated-success-{id}"));
            credential.id = Some(id);
            credential
        })
        .collect();
    store.save_credentials(&credentials).await.unwrap();
    let manager = Arc::new(
        MultiTokenManager::new_with_stores(
            Config::default(),
            credentials,
            None,
            None,
            false,
            Some(store.clone()),
            None,
        )
        .unwrap(),
    );

    let first_connection = store.pool().acquire().await.unwrap();
    let second_connection = store.pool().acquire().await.unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(CREDENTIAL_COUNT as usize + 1));
    let mut workers = Vec::with_capacity(CREDENTIAL_COUNT as usize);
    for id in 1..=CREDENTIAL_COUNT {
        let manager = manager.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            manager.report_success(id);
        }));
    }

    let started_at = Instant::now();
    barrier.wait();
    for worker in workers {
        worker.join().expect("concurrent success reporter panicked");
    }
    let elapsed = started_at.elapsed();

    assert!(
        elapsed < StdDuration::from_millis(500),
        "40 success completions must bypass a saturated PgSQL pool, elapsed={elapsed:?}"
    );
    assert_eq!(manager.runtime_mutation_backlog(), (40, 0));
    assert_eq!(manager.available_count(), CREDENTIAL_COUNT as usize);
    let route_state = manager.local_pool_route_state(None);
    assert_eq!(route_state.kind, LocalPoolRouteStateKind::Ready);
    assert_eq!(route_state.total, CREDENTIAL_COUNT as usize);
    assert_eq!(route_state.available, CREDENTIAL_COUNT as usize);
    assert!(manager.entries.lock().iter().all(|entry| {
        !entry.disabled && !entry.runtime_persistence_degraded && entry.success_count == 1
    }));

    drop(first_connection);
    drop(second_connection);
    manager.flush_pending_runtime_mutations_with_budget(StdDuration::from_secs(5));

    assert_eq!(manager.runtime_mutation_backlog(), (0, 0));
    let runtime = store.load_credential_runtime_state().await.unwrap();
    assert_eq!(runtime.len(), CREDENTIAL_COUNT as usize);
    assert!(runtime.values().all(|state| {
        state.revision == 1 && state.disabled_reason.is_none() && state.warmup_remaining == 0
    }));

    store.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_success_replay_applies_authoritative_disable_without_cancelling_in_flight() {
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };
    let mut credential = api_key_credential("pending-success-authoritative-disable");
    credential.id = Some(1);
    store.save_credentials(&[credential.clone()]).await.unwrap();
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![credential],
        None,
        None,
        false,
        Some(store.clone()),
        None,
    )
    .unwrap();
    let lease = manager
        .acquire_in_flight_lease_for_test(1)
        .expect("test credential should have capacity");

    store
        .mark_credential_disabled_at_generation(
            1,
            uuid::Uuid::new_v4(),
            0,
            DisabledReason::Manual.as_str(),
            CredentialRuntimeFailureCounts::default(),
            &Utc::now().to_rfc3339(),
        )
        .await
        .unwrap();
    let first_connection = store.pool().acquire().await.unwrap();
    let second_connection = store.pool().acquire().await.unwrap();

    manager.report_success(1);
    assert_eq!(manager.runtime_mutation_backlog().0, 1);
    assert_eq!(manager.in_flight_requests_for_test(1), 1);

    drop(first_connection);
    drop(second_connection);
    manager.flush_pending_runtime_mutations_with_budget(StdDuration::from_secs(2));

    assert_eq!(manager.runtime_mutation_backlog(), (0, 0));
    assert_eq!(
        manager.in_flight_requests_for_test(1),
        1,
        "authoritative disable must let an already-started request drain naturally"
    );
    let entry = &manager.snapshot().entries[0];
    assert!(entry.disabled);
    assert_eq!(
        entry.disabled_reason.as_deref(),
        Some(DisabledReason::Manual.as_str())
    );
    let acquire = tokio::time::timeout(StdDuration::from_secs(1), manager.acquire_context(None))
        .await
        .expect("disabled credential acquisition must not wait");
    assert!(acquire.is_err(), "disabled credential accepted new work");
    drop(lease);
    assert_eq!(manager.in_flight_requests_for_test(1), 0);

    store.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stats_shutdown_drains_frozen_and_new_stats_with_multiple_runtime_rounds() {
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };
    let mut credential = api_key_credential("stats-shutdown-drain");
    credential.id = Some(1);
    store.save_credentials(&[credential.clone()]).await.unwrap();
    let manager = Arc::new(
        MultiTokenManager::new_with_stores(
            Config::default(),
            vec![credential],
            None,
            None,
            false,
            Some(store.clone()),
            None,
        )
        .unwrap(),
    );

    manager
        .pending_stats_batches
        .lock()
        .push_back(PendingCredentialStatsBatch {
            operation_id: uuid::Uuid::new_v4(),
            deltas: HashMap::from([(
                1,
                CredentialStatsDeltaRow {
                    success_delta: 3,
                    selection_delta: 4,
                    last_used_at: Some("2025-01-01T00:00:00Z".to_string()),
                },
            )]),
        });
    manager.pending_stats_deltas.lock().insert(
        1,
        CredentialStatsDeltaRow {
            success_delta: 5,
            selection_delta: 6,
            last_used_at: Some("2026-01-01T00:00:00Z".to_string()),
        },
    );
    for _ in 0..2 {
        assert!(manager.enqueue_pending_runtime_mutation(
            1,
            PendingCredentialRuntimeMutation::Success {
                operation_id: uuid::Uuid::new_v4(),
                expected_generation: 0,
                count: 1,
            },
        ));
    }
    manager.mark_stats_dirty();

    // Delay the periodic tick so this exercises only the shutdown drain. A limit of one
    // runtime mutation per round proves that final draining crosses the normal batch boundary.
    let worker = manager.spawn_stats_flush_worker_inner(
        tokio::time::Instant::now() + StdDuration::from_secs(60),
        1,
    );
    let report = worker.shutdown(StdDuration::from_secs(10)).await;

    assert!(report.signal_sent);
    assert!(report.flushed);
    assert!(!report.timed_out);
    assert!(!report.task_failed);
    assert_eq!(report.pending_stats_batches, 0);
    assert_eq!(report.pending_stats_deltas, 0);
    assert_eq!(report.pending_runtime_mutations, 0);
    let stats = store.load_credential_stats().await.unwrap();
    let persisted = stats.get(&1).expect("both stats batches must be persisted");
    assert_eq!(persisted.success_count, 8);
    assert_eq!(persisted.selection_count, 10);
    assert_eq!(
        persisted.last_used_at.as_deref(),
        Some("2026-01-01T00:00:00Z")
    );
    let runtime = store.load_credential_runtime_state().await.unwrap();
    assert_eq!(runtime[&1].revision, 2);

    store.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn postgres_runtime_flush_round_robins_past_one_failed_credential() {
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };
    let credentials: Vec<_> = (1..=3)
        .map(|id| {
            let mut credential = api_key_credential(&format!("runtime-round-robin-{id}"));
            credential.id = Some(id);
            credential
        })
        .collect();
    store.save_credentials(&credentials).await.unwrap();
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        credentials,
        None,
        None,
        false,
        Some(store.clone()),
        None,
    )
    .unwrap();
    for id in 1..=3 {
        for _ in 0..2 {
            assert!(manager.enqueue_pending_runtime_mutation(
                id,
                PendingCredentialRuntimeMutation::Success {
                    operation_id: uuid::Uuid::new_v4(),
                    expected_generation: 0,
                    count: 1,
                },
            ));
        }
    }
    store.soft_delete_credential(1).await.unwrap();

    manager.flush_pending_runtime_mutations_with_budget(StdDuration::from_secs(2));

    {
        let pending = manager.pending_runtime_mutations.lock();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending.get(&1).map(VecDeque::len), Some(2));
    }
    let states = store.load_credential_runtime_state().await.unwrap();
    assert_eq!(states[&2].revision, 2);
    assert_eq!(states[&3].revision, 2);
    {
        let entries = manager.entries.lock();
        let failed = entries.iter().find(|entry| entry.id == 1).unwrap();
        assert!(!failed.runtime_persistence_degraded);
        assert!(!failed.disabled);
        for id in [2, 3] {
            let healthy = entries.iter().find(|entry| entry.id == id).unwrap();
            assert_eq!(healthy.runtime_revision, 2);
            assert!(!healthy.runtime_persistence_degraded);
            assert!(!healthy.disabled);
        }
    }

    manager.clear_pending_persistence_for_credential(1);
    assert!(manager.refresh_stats_dirty_from_pending());
    store.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn postgres_admin_runtime_patches_advance_revision_once_and_ignore_old_results() {
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };
    let mut credential = api_key_credential("admin-runtime-patches");
    credential.id = Some(1);
    store.save_credentials(&[credential.clone()]).await.unwrap();
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![credential],
        None,
        None,
        false,
        Some(store.clone()),
        None,
    )
    .unwrap();

    manager.set_disabled(1, true).unwrap();
    let disabled_state = store.load_credential_runtime_state().await.unwrap()[&1].clone();
    assert_eq!(disabled_state.revision, 1);
    assert_eq!(disabled_state.generation, 1);
    assert_eq!(
        disabled_state.disabled_reason.as_deref(),
        Some(DisabledReason::Manual.as_str())
    );

    manager.set_warmup_remaining(1, 7).unwrap();
    let warmup_state = store.load_credential_runtime_state().await.unwrap()[&1].clone();
    assert_eq!(warmup_state.revision, disabled_state.revision + 1);
    assert_eq!(warmup_state.generation, disabled_state.generation + 1);
    assert_eq!(warmup_state.warmup_remaining, 7);
    assert_eq!(
        warmup_state.disabled_reason.as_deref(),
        Some(DisabledReason::Manual.as_str())
    );

    manager.reset_and_enable(1).unwrap();
    let final_state = store.load_credential_runtime_state().await.unwrap()[&1].clone();
    assert_eq!(final_state.revision, warmup_state.revision + 1);
    assert_eq!(final_state.generation, warmup_state.generation + 1);
    assert_eq!(final_state.failure_count, 0);
    assert_eq!(final_state.refresh_failure_count, 0);
    assert_eq!(final_state.warmup_remaining, 7);
    assert!(final_state.disabled_reason.is_none());

    let stale_generation = final_state.generation - 1;
    assert!(!manager.persist_success_state(1, stale_generation, &Utc::now().to_rfc3339()));
    manager.persist_disabled_state(
        1,
        stale_generation,
        DisabledReason::QuotaExceeded,
        Some(MAX_FAILURES_PER_CREDENTIAL),
        None,
        &Utc::now().to_rfc3339(),
    );
    manager
        .persist_runtime_patch_best_effort_until(
            1,
            CredentialRuntimeStatePatch {
                failure_count: Some(99),
                expected_generation: Some(stale_generation),
                ..Default::default()
            },
            tokio::time::Instant::now() + StdDuration::from_secs(2),
        )
        .await;
    let after_stale_mutations = store.load_credential_runtime_state().await.unwrap()[&1].clone();
    assert_eq!(after_stale_mutations, final_state);
    assert!(
        store
            .load_credentials()
            .await
            .unwrap()
            .iter()
            .any(|credential| credential.id == Some(1) && !credential.disabled)
    );

    let applied_revisions: Vec<i64> = sqlx::query_scalar(
        "SELECT applied_revision FROM credential_runtime_mutations WHERE credential_id = $1 ORDER BY applied_revision ASC",
    )
    .bind(1_i64)
    .fetch_all(store.pool())
    .await
    .unwrap();
    assert_eq!(applied_revisions, vec![1, 2, 3]);
    {
        let entries = manager.entries.lock();
        let entry = entries.iter().find(|entry| entry.id == 1).unwrap();
        assert_eq!(entry.runtime_revision, final_state.revision);
        assert_eq!(entry.failure_count, final_state.failure_count);
        assert_eq!(
            entry.refresh_failure_count,
            final_state.refresh_failure_count
        );
        assert_eq!(entry.warmup_remaining, final_state.warmup_remaining);
        assert!(!entry.disabled);
        assert!(entry.disabled_reason.is_none());
    }

    manager.apply_runtime_mutation_result(
        1,
        &CredentialRuntimeStateMutationResult {
            state: disabled_state,
            credential_disabled: true,
            applied: true,
            credential_auto_reenabled: false,
        },
    );
    manager.apply_runtime_mutation_result(
        1,
        &CredentialRuntimeStateMutationResult {
            state: CredentialRuntimeStateRow {
                failure_count: 99,
                refresh_failure_count: 99,
                disabled_reason: Some(DisabledReason::Manual.as_str().to_string()),
                warmup_remaining: 99,
                generation: final_state.generation,
                revision: final_state.revision,
            },
            credential_disabled: false,
            applied: true,
            credential_auto_reenabled: false,
        },
    );
    {
        let entries = manager.entries.lock();
        let entry = entries.iter().find(|entry| entry.id == 1).unwrap();
        assert_eq!(entry.runtime_revision, final_state.revision);
        assert_eq!(entry.failure_count, 0);
        assert_eq!(entry.refresh_failure_count, 0);
        assert_eq!(entry.warmup_remaining, 7);
        assert!(!entry.disabled);
        assert!(entry.disabled_reason.is_none());
    }

    store.drop_test_schema().await.unwrap();
}

#[test]
fn delete_credential_clears_pending_persistence_for_that_id() {
    let mut credential = api_key_credential("delete-pending-state");
    credential.id = Some(1);
    credential.disabled = true;
    let manager =
        MultiTokenManager::new(Config::default(), vec![credential], None, None, false).unwrap();

    manager.pending_stats_deltas.lock().insert(
        1,
        CredentialStatsDeltaRow {
            success_delta: 1,
            ..CredentialStatsDeltaRow::default()
        },
    );
    assert!(manager.enqueue_pending_runtime_mutation(
        1,
        PendingCredentialRuntimeMutation::Success {
            operation_id: uuid::Uuid::new_v4(),
            expected_generation: 0,
            count: 1,
        },
    ));
    manager.mark_stats_dirty();

    manager.delete_credential(1).unwrap();

    assert!(!manager.pending_stats_deltas.lock().contains_key(&1));
    assert!(!manager.pending_runtime_mutations.lock().contains_key(&1));
    assert!(!manager.stats_dirty.load(Ordering::Acquire));
}

#[test]
fn invalid_warmup_is_rejected_without_quarantine_or_retry() {
    let mut credential = api_key_credential("invalid-warmup");
    credential.id = Some(1);
    let manager =
        MultiTokenManager::new(Config::default(), vec![credential], None, None, false).unwrap();

    let invalid = (i32::MAX as u32).saturating_add(1);
    let error = manager.set_warmup_remaining(1, invalid).unwrap_err();

    assert!(error.to_string().contains("warmup_remaining"));
    assert_eq!(manager.runtime_mutation_backlog(), (0, 0));
    let entries = manager.entries.lock();
    assert_eq!(entries[0].warmup_remaining, 0);
    assert!(!entries[0].runtime_persistence_degraded);
    assert!(!entries[0].disabled);
}

#[test]
fn startup_applies_consistent_runtime_snapshot_before_selecting_current_credential() {
    let mut first = api_key_credential("startup-runtime-first");
    first.id = Some(1);
    first.priority = 0;
    let mut second = api_key_credential("startup-runtime-second");
    second.id = Some(2);
    second.priority = 1;
    let states = HashMap::from([(
        1,
        CredentialRuntimeStateRow {
            failure_count: MAX_FAILURES_PER_CREDENTIAL,
            disabled_reason: Some(DisabledReason::QuotaExceeded.as_str().to_string()),
            revision: 1,
            ..Default::default()
        },
    )]);

    let manager = MultiTokenManager::new_with_stores_and_runtime_state(
        Config::default(),
        vec![first, second],
        None,
        None,
        None,
        Some(states),
    )
    .unwrap();

    assert_eq!(manager.current_id(), 2);
    let entries = manager.entries.lock();
    let first = entries.iter().find(|entry| entry.id == 1).unwrap();
    assert!(first.disabled);
    assert_eq!(first.disabled_reason, Some(DisabledReason::QuotaExceeded));
    assert_eq!(first.runtime_revision, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reload_remote_delete_clears_pending_persistence_for_removed_id() {
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };
    let mut first = api_key_credential("reload-remote-delete-first");
    first.id = Some(1);
    let mut removed = api_key_credential("reload-remote-delete-removed");
    removed.id = Some(2);
    store
        .save_credentials(&[first.clone(), removed.clone()])
        .await
        .unwrap();
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![first, removed],
        None,
        None,
        false,
        Some(store.clone()),
        None,
    )
    .unwrap();

    manager.pending_stats_deltas.lock().insert(
        2,
        CredentialStatsDeltaRow {
            selection_delta: 1,
            ..CredentialStatsDeltaRow::default()
        },
    );
    assert!(manager.enqueue_pending_runtime_mutation(
        2,
        PendingCredentialRuntimeMutation::Success {
            operation_id: uuid::Uuid::new_v4(),
            expected_generation: 0,
            count: 1,
        },
    ));
    manager.mark_stats_dirty();

    store.soft_delete_credential(2).await.unwrap();
    assert!(manager.reload_credentials_from_postgres().unwrap());

    assert!(!manager.entries.lock().iter().any(|entry| entry.id == 2));
    assert!(!manager.pending_stats_deltas.lock().contains_key(&2));
    assert!(!manager.pending_runtime_mutations.lock().contains_key(&2));

    store.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_mutation_cleanup_loop_drains_full_batches_and_honors_limits() {
    let outcomes = Arc::new(Mutex::new(std::collections::VecDeque::from([
        2_u64, 2, 2, 1, 9,
    ])));
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let report = cleanup_runtime_mutation_history_batches_with(2, 64, StdDuration::from_secs(1), {
        let outcomes = outcomes.clone();
        let calls = calls.clone();
        move || {
            calls.fetch_add(1, Ordering::Relaxed);
            let removed = outcomes.lock().pop_front().unwrap();
            std::future::ready(Ok(removed))
        }
    })
    .await
    .unwrap();
    assert_eq!(
        report,
        RuntimeMutationCleanupReport {
            removed: 7,
            batches: 4,
            saturated: false,
        }
    );
    assert_eq!(calls.load(Ordering::Relaxed), 4);
    assert_eq!(outcomes.lock().iter().copied().collect::<Vec<_>>(), vec![9]);

    let capped_outcomes = Arc::new(Mutex::new(std::collections::VecDeque::from([2_u64, 2, 1])));
    let capped_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let capped_report =
        cleanup_runtime_mutation_history_batches_with(2, 2, StdDuration::from_secs(1), {
            let outcomes = capped_outcomes.clone();
            let calls = capped_calls.clone();
            move || {
                calls.fetch_add(1, Ordering::Relaxed);
                let removed = outcomes.lock().pop_front().unwrap();
                std::future::ready(Ok(removed))
            }
        })
        .await
        .unwrap();
    assert_eq!(
        capped_report,
        RuntimeMutationCleanupReport {
            removed: 4,
            batches: 2,
            saturated: true,
        }
    );
    assert_eq!(capped_calls.load(Ordering::Relaxed), 2);
    assert_eq!(
        capped_outcomes.lock().iter().copied().collect::<Vec<_>>(),
        vec![1]
    );

    let budget_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let budget_report = cleanup_runtime_mutation_history_batches_with(2, 64, StdDuration::ZERO, {
        let calls = budget_calls.clone();
        move || {
            calls.fetch_add(1, Ordering::Relaxed);
            std::future::ready(Ok(0))
        }
    })
    .await
    .unwrap();
    assert_eq!(budget_report, RuntimeMutationCleanupReport::default());
    assert_eq!(budget_calls.load(Ordering::Relaxed), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_mutation_history_cleanup_drains_both_ledgers_and_is_minute_throttled() {
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };

    let mut credential = api_key_credential("runtime-mutation-cleanup");
    credential.id = Some(1);
    store
        .save_credentials(std::slice::from_ref(&credential))
        .await
        .unwrap();
    let first_operation_id = uuid::Uuid::new_v4();
    let first_operation_id_text = first_operation_id.to_string();
    let first_stats_operation_id = uuid::Uuid::new_v4();
    let first_stats_operation_id_text = first_stats_operation_id.to_string();
    store
        .record_credential_success(1, first_operation_id)
        .await
        .unwrap();
    store
        .apply_credential_stats_deltas(first_stats_operation_id, &HashMap::new())
        .await
        .unwrap();
    sqlx::query(
        "UPDATE credential_runtime_mutations SET created_at = now() - interval '31 days' WHERE operation_id = $1",
    )
    .bind(&first_operation_id_text)
    .execute(store.pool())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE credential_stats_delta_batches SET created_at = now() - interval '31 days' WHERE operation_id = $1",
    )
    .bind(&first_stats_operation_id_text)
    .execute(store.pool())
    .await
    .unwrap();

    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![credential],
        None,
        None,
        false,
        Some(store.clone()),
        None,
    )
    .unwrap();
    manager.cleanup_runtime_mutation_history_throttled();
    let first_remaining: i64 = sqlx::query_scalar(
        r#"
        SELECT (SELECT COUNT(*) FROM credential_runtime_mutations WHERE operation_id = $1)
             + (SELECT COUNT(*) FROM credential_stats_delta_batches WHERE operation_id = $2)
        "#,
    )
    .bind(&first_operation_id_text)
    .bind(&first_stats_operation_id_text)
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(first_remaining, 0);

    let second_operation_id = uuid::Uuid::new_v4();
    let second_operation_id_text = second_operation_id.to_string();
    let second_stats_operation_id = uuid::Uuid::new_v4();
    let second_stats_operation_id_text = second_stats_operation_id.to_string();
    store
        .record_credential_success(1, second_operation_id)
        .await
        .unwrap();
    store
        .apply_credential_stats_deltas(second_stats_operation_id, &HashMap::new())
        .await
        .unwrap();
    sqlx::query(
        "UPDATE credential_runtime_mutations SET created_at = now() - interval '31 days' WHERE operation_id = $1",
    )
    .bind(&second_operation_id_text)
    .execute(store.pool())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE credential_stats_delta_batches SET created_at = now() - interval '31 days' WHERE operation_id = $1",
    )
    .bind(&second_stats_operation_id_text)
    .execute(store.pool())
    .await
    .unwrap();
    manager.cleanup_runtime_mutation_history_throttled();
    let throttled_remaining: i64 = sqlx::query_scalar(
        r#"
        SELECT (SELECT COUNT(*) FROM credential_runtime_mutations WHERE operation_id = $1)
             + (SELECT COUNT(*) FROM credential_stats_delta_batches WHERE operation_id = $2)
        "#,
    )
    .bind(&second_operation_id_text)
    .bind(&second_stats_operation_id_text)
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(throttled_remaining, 2);

    *manager.last_runtime_mutation_cleanup_at.lock() = Instant::now()
        .checked_sub(CREDENTIAL_RUNTIME_MUTATION_CLEANUP_INTERVAL)
        .or(Some(Instant::now()));
    manager.cleanup_runtime_mutation_history_throttled();
    let second_remaining: i64 = sqlx::query_scalar(
        r#"
        SELECT (SELECT COUNT(*) FROM credential_runtime_mutations WHERE operation_id = $1)
             + (SELECT COUNT(*) FROM credential_stats_delta_batches WHERE operation_id = $2)
        "#,
    )
    .bind(&second_operation_id_text)
    .bind(&second_stats_operation_id_text)
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(second_remaining, 0);

    store.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_admin_updates_merge_unrelated_credential_fields() {
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };
    let mut credential = api_key_credential("credential-admin-cas-merge");
    credential.id = Some(1);
    let inserted = store.insert_credential(&credential).await.unwrap();
    let manager_a = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![inserted.clone()],
        None,
        None,
        false,
        Some(store.clone()),
        None,
    )
    .unwrap();
    let manager_b = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![inserted],
        None,
        None,
        false,
        Some(store.clone()),
        None,
    )
    .unwrap();

    manager_a.set_priority(1, 17).unwrap();
    manager_b
        .set_credential_proxy(
            1,
            None,
            Some("http://proxy.example.invalid:8080".to_string()),
            None,
            None,
        )
        .unwrap();

    let stored = store.load_credentials().await.unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].priority, 17);
    assert_eq!(
        stored[0].proxy_url.as_deref(),
        Some("http://proxy.example.invalid:8080")
    );
    let local_b = manager_b.entries.lock()[0].credentials.clone();
    assert_eq!(local_b.priority, 17);
    assert_eq!(local_b.proxy_url, stored[0].proxy_url);
    assert_eq!(local_b.storage_revision, stored[0].storage_revision);

    store.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_mutation_flush_respects_global_wall_clock_budget() {
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };
    let credentials: Vec<_> = (1..=RUNTIME_MUTATION_FLUSH_LIMIT as u64)
        .map(|id| {
            let mut credential = api_key_credential(&format!("runtime-flush-budget-{id}"));
            credential.id = Some(id);
            credential
        })
        .collect();
    store.save_credentials(&credentials).await.unwrap();
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        credentials,
        None,
        None,
        false,
        Some(store.clone()),
        None,
    )
    .unwrap();
    for id in 1..=RUNTIME_MUTATION_FLUSH_LIMIT as u64 {
        assert!(manager.enqueue_pending_runtime_mutation(
            id,
            PendingCredentialRuntimeMutation::Success {
                operation_id: uuid::Uuid::new_v4(),
                expected_generation: 0,
                count: 1,
            },
        ));
    }

    let mut transaction = store.pool().begin().await.unwrap();
    sqlx::query("LOCK TABLE credential_runtime_state IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *transaction)
        .await
        .unwrap();
    let budget = StdDuration::from_millis(150);
    let started_at = Instant::now();
    manager.flush_pending_runtime_mutations_with_budget(budget);
    let elapsed = started_at.elapsed();

    assert!(elapsed >= budget);
    assert!(elapsed < StdDuration::from_secs(1));
    assert_eq!(
        manager.runtime_mutation_backlog().0,
        RUNTIME_MUTATION_FLUSH_LIMIT
    );

    transaction.commit().await.unwrap();
    store.drop_test_schema().await.unwrap();
}

#[test]
fn token_refresh_deadlines_reserve_reconciliation_and_bound_coordination() {
    let budgets = TokenRefreshBudgets {
        workflow: StdDuration::from_millis(90),
        coordination: StdDuration::from_millis(40),
        reconciliation: StdDuration::from_millis(20),
    };
    let deadlines = budgets.deadlines().unwrap();
    assert_eq!(
        deadlines.total.duration_since(deadlines.work),
        StdDuration::from_millis(20)
    );
    assert_eq!(
        deadlines.coordination.duration_since(
            deadlines
                .total
                .checked_sub(StdDuration::from_millis(90))
                .unwrap()
        ),
        StdDuration::from_millis(40)
    );
    assert!(
        StdDuration::from_secs(TOKEN_REFRESH_REDIS_LOCK_TTL_SECS as u64)
            > TOKEN_REFRESH_WORKFLOW_TIMEOUT + REFRESH_REDIS_LOCK_OP_TIMEOUT
    );
    assert!(
        refresh_step_timeout(
            tokio::time::Instant::now() - StdDuration::from_millis(1),
            REFRESH_REDIS_LOCK_OP_TIMEOUT,
        )
        .is_none()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_step_deadline_preempts_synchronous_first_poll_work() {
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let started_at = Instant::now();
    let deadline = tokio::time::Instant::now() + StdDuration::from_millis(100);
    let step = tokio::spawn(async move {
        run_refresh_step_until("同步冷启动测试", deadline, async move {
            let _ = started_tx.send(());
            std::thread::sleep(StdDuration::from_millis(500));
            std::future::pending::<anyhow::Result<()>>().await
        })
        .await
    });

    tokio::time::timeout(StdDuration::from_secs(1), started_rx)
        .await
        .expect("同步首次 poll 未开始")
        .expect("同步首次 poll 通知发送端提前关闭");
    let error = tokio::time::timeout(StdDuration::from_millis(300), step)
        .await
        .expect("refresh step deadline 被同步首次 poll 穿透")
        .expect("refresh step 测试任务异常")
        .unwrap_err();
    assert!(error.to_string().contains("工作期限"));
    assert!(started_at.elapsed() < StdDuration::from_millis(300));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn force_refresh_local_lock_wait_respects_coordination_deadline() {
    let credential = force_refresh_test_credential("http://127.0.0.1:1/token".to_string());
    let manager =
        MultiTokenManager::new(Config::default(), vec![credential], None, None, false).unwrap();
    let refresh_lock = manager.refresh_lock_for_credential(1);
    let _held = refresh_lock.lock().await;
    let started_at = Instant::now();
    let error = manager
        .force_refresh_token_for_with_budgets(
            1,
            TokenRefreshBudgets {
                workflow: StdDuration::from_millis(150),
                coordination: StdDuration::from_millis(40),
                reconciliation: StdDuration::from_millis(50),
            },
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("本地 Token 刷新锁超时"));
    assert!(started_at.elapsed() < StdDuration::from_millis(250));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_refresh_peer_wait_respects_coordination_deadline() {
    let (Some(store), Some(redis)) = (test_postgres_store().await, test_redis_store().await) else {
        eprintln!("跳过 PgSQL/Redis TokenManager 集成测试：未设置存储集成测试环境变量");
        return;
    };
    let mut credential = force_refresh_test_credential("http://127.0.0.1:1/token".to_string());
    credential.expires_at = Some((Utc::now() - Duration::hours(1)).to_rfc3339());
    let inserted = store.insert_credential(&credential).await.unwrap();
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![inserted.clone()],
        None,
        None,
        false,
        Some(store.clone()),
        Some(redis.clone()),
    )
    .unwrap();
    let peer_lock = redis.acquire_refresh_lock(1, 30).await.unwrap().unwrap();

    let started_at = Instant::now();
    let result = manager
        .try_ensure_token_with_budgets(
            1,
            &inserted,
            true,
            TokenRefreshBudgets {
                workflow: StdDuration::from_millis(250),
                coordination: StdDuration::from_millis(80),
                reconciliation: StdDuration::from_millis(50),
            },
        )
        .await;
    let error = match result {
        Ok(_) => panic!("peer refresh wait unexpectedly succeeded"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("期限") || error.to_string().contains("超时"));
    assert!(started_at.elapsed() < StdDuration::from_millis(300));

    assert!(redis.release_refresh_lock(1, &peer_lock).await.unwrap());
    store.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_force_refresh_respects_total_deadline_and_releases_redis_lock() {
    let Some(redis) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let (token_endpoint, request_received, server) = spawn_pending_refresh_token_endpoint().await;
    let credential = force_refresh_test_credential(token_endpoint);
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![credential],
        None,
        None,
        false,
        None,
        Some(redis.clone()),
    )
    .unwrap();

    let started_at = Instant::now();
    let refresh = manager.force_refresh_token_for_with_budgets(
        1,
        TokenRefreshBudgets {
            workflow: StdDuration::from_secs(4),
            coordination: StdDuration::from_secs(1),
            reconciliation: StdDuration::from_millis(500),
        },
    );
    tokio::pin!(refresh);
    tokio::time::timeout(StdDuration::from_secs(3), async {
        tokio::select! {
            _ = request_received.notified() => {}
            result = &mut refresh => panic!("强制刷新在请求 pending Token endpoint 前结束: {result:?}"),
        }
    })
    .await
    .expect("pending Token endpoint 未在测试期限内收到请求");
    let result = tokio::time::timeout(StdDuration::from_secs(5), &mut refresh)
        .await
        .expect("强制刷新未在总期限后结束");
    assert!(result.unwrap_err().to_string().contains("工作期限"));
    assert!(started_at.elapsed() < StdDuration::from_secs(5));
    let next_lock = redis.acquire_refresh_lock(1, 30).await.unwrap().unwrap();
    assert!(redis.release_refresh_lock(1, &next_lock).await.unwrap());

    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aborting_force_refresh_submits_critical_redis_lock_cleanup() {
    let Some(redis) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let (token_endpoint, request_received, server) = spawn_pending_refresh_token_endpoint().await;
    let credential = force_refresh_test_credential(token_endpoint);
    let manager = Arc::new(
        MultiTokenManager::new_with_stores(
            Config::default(),
            vec![credential],
            None,
            None,
            false,
            None,
            Some(redis.clone()),
        )
        .unwrap(),
    );
    let mut refresh_task = {
        let manager = manager.clone();
        tokio::spawn(async move {
            manager
                .force_refresh_token_for_with_budgets(
                    1,
                    TokenRefreshBudgets {
                        workflow: StdDuration::from_secs(10),
                        coordination: StdDuration::from_secs(3),
                        reconciliation: StdDuration::from_secs(1),
                    },
                )
                .await
        })
    };
    tokio::time::timeout(StdDuration::from_secs(5), async {
        tokio::select! {
            _ = request_received.notified() => {}
            result = &mut refresh_task => {
                panic!("强制刷新在请求 pending Token endpoint 前结束: {result:?}")
            }
        }
    })
    .await
    .expect("pending Token endpoint 未在测试期限内收到强制刷新请求");
    assert!(redis.acquire_refresh_lock(1, 30).await.unwrap().is_none());

    refresh_task.abort();
    let _ = refresh_task.await;
    let next_lock = acquire_test_refresh_lock_until(
        redis.as_ref(),
        1,
        tokio::time::Instant::now() + StdDuration::from_secs(1),
    )
    .await;
    assert!(redis.release_refresh_lock(1, &next_lock).await.unwrap());

    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aborting_ordinary_refresh_submits_critical_redis_lock_cleanup() {
    let Some(redis) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let (token_endpoint, request_received, server) = spawn_pending_refresh_token_endpoint().await;
    let mut credential = force_refresh_test_credential(token_endpoint);
    credential.expires_at = Some((Utc::now() - Duration::hours(1)).to_rfc3339());
    let refresh_credentials = credential.clone();
    let manager = Arc::new(
        MultiTokenManager::new_with_stores(
            Config::default(),
            vec![credential],
            None,
            None,
            false,
            None,
            Some(redis.clone()),
        )
        .unwrap(),
    );
    let mut refresh_task = {
        let manager = manager.clone();
        tokio::spawn(async move {
            manager
                .try_ensure_token_with_budgets(
                    1,
                    &refresh_credentials,
                    false,
                    TokenRefreshBudgets {
                        workflow: StdDuration::from_secs(10),
                        coordination: StdDuration::from_secs(3),
                        reconciliation: StdDuration::from_secs(1),
                    },
                )
                .await
        })
    };
    tokio::time::timeout(StdDuration::from_secs(5), async {
        tokio::select! {
            _ = request_received.notified() => {}
            result = &mut refresh_task => match result {
                Ok(Ok(_)) => panic!("普通刷新在请求 pending Token endpoint 前成功"),
                Ok(Err(error)) => {
                    panic!("普通刷新在请求 pending Token endpoint 前失败: {error:#}")
                }
                Err(error) => panic!("普通刷新任务在请求 pending Token endpoint 前终止: {error}"),
            },
        }
    })
    .await
    .expect("pending Token endpoint 未在测试期限内收到普通刷新请求");
    assert!(redis.acquire_refresh_lock(1, 30).await.unwrap().is_none());

    refresh_task.abort();
    let _ = refresh_task.await;
    let next_lock = acquire_test_refresh_lock_until(
        redis.as_ref(),
        1,
        tokio::time::Instant::now() + StdDuration::from_secs(1),
    )
    .await;
    assert!(redis.release_refresh_lock(1, &next_lock).await.unwrap());

    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn force_refresh_runtime_reset_cannot_overrun_total_deadline() {
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };
    let (token_endpoint, _request_received, server) = spawn_force_refresh_token_endpoint().await;
    let credential = force_refresh_test_credential(token_endpoint);
    refresh_token(&credential, &Config::default(), None)
        .await
        .unwrap();
    let inserted = store.insert_credential(&credential).await.unwrap();
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![inserted],
        None,
        None,
        false,
        Some(store.clone()),
        None,
    )
    .unwrap();
    sqlx::query(
        "INSERT INTO credential_runtime_state (credential_id) VALUES ($1) ON CONFLICT DO NOTHING",
    )
    .bind(1_i64)
    .execute(store.pool())
    .await
    .unwrap();
    let mut transaction = store.pool().begin().await.unwrap();
    let locked_credential_id: i64 = sqlx::query_scalar(
        "SELECT credential_id FROM credential_runtime_state WHERE credential_id = $1 FOR UPDATE",
    )
    .bind(1_i64)
    .fetch_one(&mut *transaction)
    .await
    .unwrap();
    assert_eq!(locked_credential_id, 1);

    let started_at = Instant::now();
    tokio::time::timeout(
        StdDuration::from_secs(2),
        manager.force_refresh_token_for_with_budgets(
            1,
            TokenRefreshBudgets {
                workflow: StdDuration::from_secs(1),
                coordination: StdDuration::from_millis(100),
                reconciliation: StdDuration::from_millis(200),
            },
        ),
    )
    .await
    .expect("被锁定的 runtime 写回不得拖住强制刷新工作流")
    .unwrap();
    assert!(started_at.elapsed() < StdDuration::from_secs(2));
    assert_eq!(manager.runtime_mutation_backlog().0, 1);

    transaction.commit().await.unwrap();
    manager.flush_pending_runtime_mutations_with_budget(StdDuration::from_secs(1));
    assert_eq!(manager.runtime_mutation_backlog().0, 0);
    server.abort();
    tokio::time::timeout(StdDuration::from_secs(5), async {
        loop {
            let event_recorded: bool = sqlx::query_scalar(
                r#"
                SELECT EXISTS(
                    SELECT 1
                    FROM credential_events
                    WHERE credential_id = 1
                      AND reason = 'credential_force_refreshed'
                )
                "#,
            )
            .fetch_one(store.pool())
            .await
            .unwrap();
            if event_recorded {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("强制刷新事件未在测试期限内完成异步持久化");
    store.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn force_refresh_holds_redis_lock_until_postgres_commit() {
    let (Some(store), Some(redis)) = (test_postgres_store().await, test_redis_store().await) else {
        eprintln!("跳过 PgSQL/Redis TokenManager 集成测试：未设置存储集成测试环境变量");
        return;
    };
    let (token_endpoint, request_received, server) = spawn_force_refresh_token_endpoint().await;
    let credential = force_refresh_test_credential(token_endpoint);
    store
        .save_credentials(std::slice::from_ref(&credential))
        .await
        .unwrap();
    let manager = Arc::new(
        MultiTokenManager::new_with_stores(
            Config::default(),
            vec![credential],
            None,
            None,
            false,
            Some(store.clone()),
            Some(redis.clone()),
        )
        .unwrap(),
    );

    let mut transaction = store.pool().begin().await.unwrap();
    sqlx::query("LOCK TABLE credentials IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *transaction)
        .await
        .unwrap();
    let refresh_task = {
        let manager = manager.clone();
        tokio::spawn(async move { manager.force_refresh_token_for(1).await })
    };
    request_received.notified().await;
    for _ in 0..20 {
        assert!(!refresh_task.is_finished());
        if let Some(contender_lock) = redis.acquire_refresh_lock(1, 30).await.unwrap() {
            let _ = redis.release_refresh_lock(1, &contender_lock).await;
            panic!("PgSQL upsert 阻塞期间 Redis 刷新锁已提前释放");
        }
        tokio::time::sleep(StdDuration::from_millis(25)).await;
    }

    transaction.commit().await.unwrap();
    tokio::time::timeout(StdDuration::from_secs(10), refresh_task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let next_lock = redis.acquire_refresh_lock(1, 30).await.unwrap().unwrap();
    assert!(redis.release_refresh_lock(1, &next_lock).await.unwrap());

    let local_access_token = manager.entries.lock()[0].credentials.access_token.clone();
    assert_eq!(
        local_access_token.as_deref(),
        Some("force-refreshed-access-token")
    );
    let stored = store.load_credentials().await.unwrap();
    assert_eq!(
        stored[0].access_token.as_deref(),
        Some("force-refreshed-access-token")
    );

    server.abort();
    crate::kiro::token_manager::drain_best_effort_storage_tasks(StdDuration::from_secs(1)).await;
    store.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn force_refresh_postgres_failure_returns_error_without_updating_local_credentials() {
    let (Some(store), Some(redis)) = (test_postgres_store().await, test_redis_store().await) else {
        eprintln!("跳过 PgSQL/Redis TokenManager 集成测试：未设置存储集成测试环境变量");
        return;
    };
    let (token_endpoint, _request_received, server) = spawn_force_refresh_token_endpoint().await;
    let credential = force_refresh_test_credential(token_endpoint);
    store
        .save_credentials(std::slice::from_ref(&credential))
        .await
        .unwrap();
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![credential],
        None,
        None,
        false,
        Some(store.clone()),
        Some(redis.clone()),
    )
    .unwrap();
    sqlx::query("ALTER TABLE credentials RENAME TO credentials_force_refresh_backup")
        .execute(store.pool())
        .await
        .unwrap();
    sqlx::query("CREATE TABLE credentials (id BIGINT PRIMARY KEY)")
        .execute(store.pool())
        .await
        .unwrap();

    let error = manager.force_refresh_token_for(1).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("强制刷新 Token 后写入 PgSQL 失败")
    );
    let expected_refresh_token = "r".repeat(150);
    {
        let entries = manager.entries.lock();
        let entry = &entries[0];
        assert_eq!(
            entry.credentials.access_token.as_deref(),
            Some("old-access-token")
        );
        assert_eq!(
            entry.credentials.refresh_token.as_deref(),
            Some(expected_refresh_token.as_str())
        );
    }
    let next_lock = redis.acquire_refresh_lock(1, 30).await.unwrap().unwrap();
    assert!(redis.release_refresh_lock(1, &next_lock).await.unwrap());

    server.abort();
    store.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_cas_treats_already_committed_fields_as_success() {
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };
    let credential =
        force_refresh_test_credential("http://127.0.0.1:1/token-that-is-never-called".to_string());
    let inserted = store.insert_credential(&credential).await.unwrap();
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![inserted.clone()],
        None,
        None,
        false,
        Some(store.clone()),
        None,
    )
    .unwrap();
    let mut refreshed = inserted.clone();
    refreshed.access_token = Some("commit-unknown-new-access-token".to_string());
    refreshed.refresh_token = Some("n".repeat(150));
    refreshed.expires_at = Some((Utc::now() + chrono::Duration::hours(1)).to_rfc3339());
    let expected = CredentialRefreshExpectedContext::from_credentials(&inserted).unwrap();
    let patch = CredentialRefreshFieldsPatch {
        access_token: refreshed.access_token.clone(),
        refresh_token: refreshed.refresh_token.clone(),
        profile_arn: refreshed.profile_arn.clone(),
        expires_at: refreshed.expires_at.clone(),
        scopes: refreshed.scopes.clone(),
    };
    let first = store
        .update_credential_refresh_fields_cas(1, &expected, &patch)
        .await
        .unwrap();
    assert!(matches!(
        first,
        CredentialRefreshFieldsCasOutcome::Applied(_)
    ));

    let reconciliation_started_at = tokio::time::Instant::now();
    let recovered = manager
        .persist_refreshed_credential_fields(
            1,
            &inserted,
            refreshed,
            false,
            reconciliation_started_at,
            reconciliation_started_at + StdDuration::from_secs(5),
        )
        .await
        .unwrap();
    assert_eq!(
        recovered.access_token.as_deref(),
        Some("commit-unknown-new-access-token")
    );
    assert_eq!(
        recovered.refresh_token.as_deref(),
        Some("n".repeat(150).as_str())
    );
    assert!(recovered.storage_revision > inserted.storage_revision);

    store.drop_test_schema().await.unwrap();
}

#[test]
fn test_partial_scheduler_state_apply_preserves_unrequested_entries() {
    let manager = MultiTokenManager::new(
        Config::default(),
        vec![
            test_access_token_credential("token-1", "Pro"),
            test_access_token_credential("token-2", "Pro"),
        ],
        None,
        None,
        false,
    )
    .unwrap();

    {
        let mut entries = manager.entries.lock();
        entries[0].in_flight_requests = 7;
        entries[0].health.last_error_kind = Some("old-first".to_string());
        entries[1].in_flight_requests = 3;
        entries[1].health.last_error_kind = Some("keep-second".to_string());
        entries[1].health.selection_count = 11;
    }

    let mut health = SchedulerHealthState {
        last_error_kind: Some("updated-first".to_string()),
        selection_count: 5,
        ..Default::default()
    };
    health.recent_selection_count_60s = 2;
    let mut states = HashMap::new();
    states.insert(
        1,
        SchedulerCredentialState {
            health,
            ..Default::default()
        },
    );

    manager.apply_scheduler_states_for_ids(states);

    let entries = manager.entries.lock();
    assert_eq!(
        entries[0].health.last_error_kind.as_deref(),
        Some("updated-first")
    );
    assert_eq!(entries[0].health.selection_count, 5);
    assert_eq!(entries[0].in_flight_requests, 0);
    assert_eq!(
        entries[1].health.last_error_kind.as_deref(),
        Some("keep-second")
    );
    assert_eq!(entries[1].health.selection_count, 11);
    assert_eq!(entries[1].in_flight_requests, 3);
}

#[test]
fn test_scheduler_state_apply_preserves_local_in_flight_leases() {
    let manager = MultiTokenManager::new(
        Config::default(),
        vec![test_access_token_credential("token-1", "Pro")],
        None,
        None,
        false,
    )
    .unwrap();

    let now = Instant::now();
    {
        let mut entries = manager.entries.lock();
        entries[0].in_flight_requests = 1;
        entries[0].in_flight_leases.push(InFlightLease {
            id: 99,
            acquired_at: now,
            last_seen_at: now,
            kind: InFlightKind::Stream,
            weight_units: 1,
            locally_owned: true,
        });
    }

    let mut states = HashMap::new();
    states.insert(1, SchedulerCredentialState::default());

    manager.apply_scheduler_states_for_ids(states);

    let entries = manager.entries.lock();
    assert_eq!(entries[0].in_flight_requests, 1);
    assert_eq!(entries[0].in_flight_leases.len(), 1);
    assert_eq!(entries[0].in_flight_leases[0].id, 99);
    assert_eq!(entries[0].in_flight_leases[0].kind, InFlightKind::Stream);
}

#[test]
fn test_scheduler_state_apply_filters_expired_redis_in_flight_leases() {
    let mut config = Config::default();
    config.credential_in_flight_lease_max_secs = 1;
    let manager = MultiTokenManager::new(
        config,
        vec![test_access_token_credential("token-1", "Pro")],
        None,
        None,
        false,
    )
    .unwrap();
    let now_ms = Utc::now().timestamp_millis();
    let stale_redis_lease = crate::storage::redis_cache::SchedulerInFlightLease {
        id: 44,
        acquired_at_ms: now_ms - 5_000,
        last_seen_at_ms: now_ms - 5_000,
        kind: InFlightKind::Api.as_str().to_string(),
        weight_units: 1,
    };

    let mut states = HashMap::new();
    states.insert(
        1,
        SchedulerCredentialState {
            in_flight_leases: vec![stale_redis_lease.clone()],
            ..Default::default()
        },
    );
    manager.apply_scheduler_states_for_ids(states);
    {
        let entries = manager.entries.lock();
        assert_eq!(entries[0].in_flight_requests, 0);
        assert!(entries[0].in_flight_leases.is_empty());
    }

    let now = Instant::now();
    {
        let mut entries = manager.entries.lock();
        entries[0].in_flight_requests = 1;
        entries[0].in_flight_leases.push(InFlightLease {
            id: 99,
            acquired_at: now,
            last_seen_at: now,
            kind: InFlightKind::Stream,
            weight_units: 1,
            locally_owned: true,
        });
    }

    let mut states = HashMap::new();
    states.insert(
        1,
        SchedulerCredentialState {
            in_flight_leases: vec![stale_redis_lease],
            ..Default::default()
        },
    );
    manager.apply_scheduler_states_for_ids(states);

    let entries = manager.entries.lock();
    assert_eq!(entries[0].in_flight_requests, 1);
    assert_eq!(entries[0].in_flight_leases.len(), 1);
    assert_eq!(entries[0].in_flight_leases[0].id, 99);
    assert_eq!(entries[0].in_flight_leases[0].kind, InFlightKind::Stream);
}

#[test]
fn test_scheduler_state_apply_ignores_recently_released_redis_in_flight_lease() {
    let manager = MultiTokenManager::new(
        Config::default(),
        vec![test_access_token_credential("token-1", "Pro")],
        None,
        None,
        false,
    )
    .unwrap();
    let now_ms = Utc::now().timestamp_millis();

    record_released_in_flight_lease_tombstone(&manager.released_in_flight_lease_tombstones, 1, 44);

    let mut states = HashMap::new();
    states.insert(
        1,
        SchedulerCredentialState {
            in_flight_leases: vec![crate::storage::redis_cache::SchedulerInFlightLease {
                id: 44,
                acquired_at_ms: now_ms,
                last_seen_at_ms: now_ms,
                kind: InFlightKind::Api.as_str().to_string(),
                weight_units: 1,
            }],
            ..Default::default()
        },
    );
    manager.apply_scheduler_states_for_ids(states);
    {
        let entries = manager.entries.lock();
        assert_eq!(entries[0].in_flight_requests, 0);
        assert!(entries[0].in_flight_leases.is_empty());
    }

    let mut states = HashMap::new();
    states.insert(
        1,
        SchedulerCredentialState {
            in_flight_leases: vec![crate::storage::redis_cache::SchedulerInFlightLease {
                id: 45,
                acquired_at_ms: now_ms,
                last_seen_at_ms: now_ms,
                kind: InFlightKind::Api.as_str().to_string(),
                weight_units: 1,
            }],
            ..Default::default()
        },
    );
    manager.apply_scheduler_states_for_ids(states);

    let entries = manager.entries.lock();
    assert_eq!(entries[0].in_flight_requests, 1);
    assert_eq!(entries[0].in_flight_leases.len(), 1);
    assert_eq!(entries[0].in_flight_leases[0].id, 45);
}

#[test]
fn test_scheduler_state_apply_drops_remote_lease_missing_from_next_snapshot() {
    let manager = MultiTokenManager::new(
        Config::default(),
        vec![test_access_token_credential("token-1", "Pro")],
        None,
        None,
        false,
    )
    .unwrap();
    let now_ms = Utc::now().timestamp_millis();
    let mut states = HashMap::new();
    states.insert(
        1,
        SchedulerCredentialState {
            in_flight_leases: vec![crate::storage::redis_cache::SchedulerInFlightLease {
                id: 77,
                acquired_at_ms: now_ms,
                last_seen_at_ms: now_ms,
                kind: InFlightKind::Api.as_str().to_string(),
                weight_units: 1,
            }],
            ..Default::default()
        },
    );
    manager.apply_scheduler_states_for_ids(states);
    {
        let entries = manager.entries.lock();
        assert_eq!(entries[0].in_flight_requests, 1);
        assert!(!entries[0].in_flight_leases[0].locally_owned);
    }

    let mut states = HashMap::new();
    states.insert(1, SchedulerCredentialState::default());
    manager.apply_scheduler_states_for_ids(states);

    let entries = manager.entries.lock();
    assert_eq!(entries[0].in_flight_requests, 0);
    assert!(entries[0].in_flight_leases.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn scheduler_non_admission_timeout_does_not_degrade_admission() {
    let manager = MultiTokenManager::new(
        Config::default(),
        vec![test_access_token_credential("token-1", "Pro")],
        None,
        None,
        false,
    )
    .unwrap();

    let result = manager
        .block_on_scheduler_redis_non_admission("测试 Redis 非准入操作", async move {
            std::future::pending::<anyhow::Result<()>>().await
        });

    assert!(result.is_none());
    assert!(manager.scheduler_redis_degraded_until.lock().is_none());
    assert_eq!(
        manager
            .scheduler_redis_degraded_streak
            .load(Ordering::Acquire),
        0
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn scheduler_non_admission_success_does_not_reset_admission_streak() {
    let manager = MultiTokenManager::new(
        Config::default(),
        vec![test_access_token_credential("token-1", "Pro")],
        None,
        None,
        false,
    )
    .unwrap();
    manager
        .scheduler_redis_degraded_streak
        .store(3, Ordering::Release);

    let result = manager
        .block_on_scheduler_redis_non_admission("测试 Redis 非准入成功", async move {
            Ok::<_, anyhow::Error>(7_u32)
        });

    assert_eq!(result, Some(7));
    assert_eq!(
        manager
            .scheduler_redis_degraded_streak
            .load(Ordering::Acquire),
        3
    );
    assert!(manager.scheduler_redis_degraded_until.lock().is_none());
}

#[test]
fn recent_confirmed_admission_success_prevents_failure_cluster_from_opening_breaker() {
    let manager = MultiTokenManager::new(
        Config::default(),
        vec![test_access_token_credential("token-1", "Pro")],
        None,
        None,
        false,
    )
    .unwrap();
    manager.mark_scheduler_redis_admission_healthy();

    let err = anyhow::anyhow!("redis connection closed");
    for index in 0..SCHEDULER_REDIS_ADMISSION_FAILURE_THRESHOLD + 2 {
        if index % 2 == 0 {
            manager.record_scheduler_redis_admission_timeout("测试并发 Redis 准入硬超时");
        } else {
            manager.record_scheduler_redis_admission_failure(
                "测试并发 Redis 准入错误",
                "error",
                &err,
            );
        }
    }
    assert!(
        manager.scheduler_redis_degraded_until.lock().is_none(),
        "failures interleaved with a recent authoritative success must not fan out into a whole-pool breaker"
    );

    *manager.last_scheduler_redis_admission_success_at.lock() = Some(
        Instant::now() - SCHEDULER_REDIS_ADMISSION_NO_SUCCESS_WINDOW - StdDuration::from_millis(1),
    );
    manager.record_scheduler_redis_admission_failure("测试持续 Redis 准入错误", "error", &err);
    assert!(manager.scheduler_redis_degraded_until.lock().is_some());
}

#[test]
fn admission_failures_completed_before_newer_success_are_ignored() {
    let manager = MultiTokenManager::new(
        Config::default(),
        vec![test_access_token_credential("token-1", "Pro")],
        None,
        None,
        false,
    )
    .unwrap();
    let failure_completed_at = Instant::now();
    manager.mark_scheduler_redis_admission_healthy();
    let err = anyhow::anyhow!("redis connection closed");

    for _ in 0..SCHEDULER_REDIS_ADMISSION_FAILURE_THRESHOLD + 2 {
        manager.record_scheduler_redis_admission_failure_at(
            "测试晚提交的旧 Redis 准入失败",
            "error",
            &err,
            failure_completed_at,
        );
    }

    assert_eq!(
        manager
            .scheduler_redis_admission_failure_streak
            .load(Ordering::Acquire),
        0
    );
    assert!(manager.scheduler_redis_degraded_until.lock().is_none());
}

#[test]
fn redis_failure_message_uses_owner_time_when_caller_resumes_after_newer_success() {
    let manager = MultiTokenManager::new(
        Config::default(),
        vec![test_access_token_credential("token-1", "Pro")],
        None,
        None,
        false,
    )
    .unwrap();
    let completed_at = Instant::now() - StdDuration::from_millis(10);
    let started_at = completed_at - StdDuration::from_millis(10);
    let delayed_failure = RedisAdmissionTaskMessage::RedisFailed {
        err: anyhow::anyhow!("redis connection closed"),
        started_at,
        completed_at,
    };

    manager.mark_scheduler_redis_admission_healthy();
    let outcome = manager.record_redis_dispatch_admission_failure_message(
        "测试 caller 延迟处理旧 Redis 失败消息",
        delayed_failure,
    );

    assert!(matches!(outcome, SchedulerRedisHotOutcome::Failed));
    assert_eq!(
        manager
            .scheduler_redis_admission_failure_streak
            .load(Ordering::Acquire),
        0,
        "an old owner failure processed after a newer success must not resurrect the streak"
    );
    assert!(manager.scheduler_redis_degraded_until.lock().is_none());
}

#[test]
fn concurrent_admission_failure_wave_advances_breaker_once() {
    let manager = MultiTokenManager::new(
        Config::default(),
        vec![test_access_token_credential("token-1", "Pro")],
        None,
        None,
        false,
    )
    .unwrap();
    *manager.last_scheduler_redis_admission_success_at.lock() = Some(
        Instant::now() - SCHEDULER_REDIS_ADMISSION_NO_SUCCESS_WINDOW - StdDuration::from_secs(1),
    );
    let wave_started_at = Instant::now();
    let err = anyhow::anyhow!("redis connection closed");

    for offset_ms in 1..=64 {
        manager.record_scheduler_redis_admission_failure_during(
            "测试并发 Redis 准入失败波次",
            "error",
            &err,
            wave_started_at,
            wave_started_at + StdDuration::from_millis(offset_ms),
        );
    }

    assert_eq!(
        manager
            .scheduler_redis_admission_failure_streak
            .load(Ordering::Acquire),
        1,
        "one overlapping caller wave must advance the breaker only once"
    );
    assert!(manager.scheduler_redis_degraded_until.lock().is_none());
}

#[tokio::test]
async fn expired_scheduler_breaker_allows_only_one_half_open_probe() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![test_access_token_credential("token-1", "Pro")],
        None,
        None,
        false,
        None,
        Some(redis_store),
    )
    .unwrap();
    *manager.scheduler_redis_degraded_until.lock() =
        Some(Instant::now() - StdDuration::from_millis(1));

    let probe = manager
        .scheduler_redis_admission_permit(true)
        .expect("expired breaker must allow one probe");
    assert!(
        manager.scheduler_redis_admission_permit(true).is_none(),
        "a second caller must not join an in-flight half-open probe"
    );
    drop(probe);

    let recovered = manager
        .await_scheduler_redis_admission_outcome("测试 Redis half-open 恢复", true, async {
            Ok::<_, anyhow::Error>(7_u32)
        })
        .await;
    assert!(matches!(recovered, SchedulerRedisHotOutcome::Completed(7)));
    assert!(manager.scheduler_redis_degraded_until.lock().is_none());
    assert_eq!(
        manager
            .scheduler_redis_admission_failure_streak
            .load(Ordering::Acquire),
        0
    );
}

#[tokio::test]
async fn queue_coordination_success_does_not_mask_dispatch_admission_timeouts() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![test_access_token_credential("token-1", "Pro")],
        None,
        None,
        false,
        None,
        Some(redis_store),
    )
    .unwrap();
    let old_success =
        Instant::now() - SCHEDULER_REDIS_ADMISSION_NO_SUCCESS_WINDOW - StdDuration::from_millis(1);
    *manager.last_scheduler_redis_admission_success_at.lock() = Some(old_success);
    manager.scheduler_redis_admission_failure_streak.store(
        SCHEDULER_REDIS_ADMISSION_FAILURE_THRESHOLD - 1,
        Ordering::Release,
    );

    let result = manager
        .await_scheduler_redis_admission_outcome("测试 Redis 队列协调成功", false, async move {
            Ok::<_, anyhow::Error>(true)
        })
        .await;
    assert!(matches!(result, SchedulerRedisHotOutcome::Completed(true)));
    assert_eq!(
        *manager.last_scheduler_redis_admission_success_at.lock(),
        Some(old_success)
    );
    assert_eq!(
        manager
            .scheduler_redis_admission_failure_streak
            .load(Ordering::Acquire),
        SCHEDULER_REDIS_ADMISSION_FAILURE_THRESHOLD - 1
    );
    for _ in 0..SCHEDULER_REDIS_ADMISSION_FAILURE_THRESHOLD {
        let queue_timeout = manager
            .await_scheduler_redis_admission_outcome("测试 Redis 队列协调超时", false, async move {
                std::future::pending::<anyhow::Result<bool>>().await
            })
            .await;
        assert!(matches!(queue_timeout, SchedulerRedisHotOutcome::Failed));
    }
    assert!(manager.scheduler_redis_degraded_until.lock().is_none());
    assert_eq!(
        manager
            .scheduler_redis_admission_failure_streak
            .load(Ordering::Acquire),
        SCHEDULER_REDIS_ADMISSION_FAILURE_THRESHOLD - 1
    );

    manager.record_scheduler_redis_admission_timeout("测试持续 Redis dispatch 准入超时");
    assert!(manager.scheduler_redis_degraded_until.lock().is_some());
}

#[tokio::test]
async fn scheduler_admission_soft_budget_overrun_keeps_admission_healthy() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![test_access_token_credential("token-1", "Pro")],
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();

    let result = manager
        .await_scheduler_redis_admission_outcome("测试 Redis 准入软延迟", true, async move {
            tokio::time::sleep(SCHEDULER_REDIS_HOT_OP_TIMEOUT + StdDuration::from_millis(25)).await;
            Ok::<_, anyhow::Error>(7_u32)
        })
        .await;

    assert!(matches!(result, SchedulerRedisHotOutcome::Completed(7)));
    assert!(manager.scheduler_redis_degraded_until.lock().is_none());
    assert_eq!(
        manager
            .scheduler_redis_degraded_streak
            .load(Ordering::Acquire),
        0
    );
}

#[tokio::test]
async fn scheduler_admission_requires_three_consecutive_hard_timeouts_to_open_breaker() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![test_access_token_credential("token-1", "Pro")],
        None,
        None,
        false,
        None,
        Some(redis_store),
    )
    .unwrap();

    for expected_streak in 1..SCHEDULER_REDIS_ADMISSION_FAILURE_THRESHOLD {
        let result = manager
            .await_scheduler_redis_admission_outcome("测试 Redis 准入硬超时", true, async move {
                std::future::pending::<anyhow::Result<()>>().await
            })
            .await;
        assert!(matches!(result, SchedulerRedisHotOutcome::Failed));
        assert!(manager.scheduler_redis_degraded_until.lock().is_none());
        assert_eq!(
            manager
                .scheduler_redis_admission_failure_streak
                .load(Ordering::Acquire),
            expected_streak
        );
    }
    let result = manager
        .await_scheduler_redis_admission_outcome("测试 Redis 准入硬超时", true, async move {
            std::future::pending::<anyhow::Result<()>>().await
        })
        .await;
    assert!(matches!(result, SchedulerRedisHotOutcome::Failed));
    assert!(manager.scheduler_redis_degraded_until.lock().is_some());
}

#[tokio::test]
async fn scheduler_admission_requires_three_consecutive_errors_to_open_breaker() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![test_access_token_credential("token-1", "Pro")],
        None,
        None,
        false,
        None,
        Some(redis_store),
    )
    .unwrap();

    for expected_streak in 1..SCHEDULER_REDIS_ADMISSION_FAILURE_THRESHOLD {
        let result = manager
            .await_scheduler_redis_admission_outcome("测试 Redis 准入错误", true, async move {
                Err::<(), _>(anyhow::anyhow!("redis connection closed"))
            })
            .await;
        assert!(matches!(result, SchedulerRedisHotOutcome::Failed));
        assert!(manager.scheduler_redis_degraded_until.lock().is_none());
        assert_eq!(
            manager
                .scheduler_redis_admission_failure_streak
                .load(Ordering::Acquire),
            expected_streak
        );
    }
    let result = manager
        .await_scheduler_redis_admission_outcome("测试 Redis 准入错误", true, async move {
            Err::<(), _>(anyhow::anyhow!("redis connection closed"))
        })
        .await;
    assert!(matches!(result, SchedulerRedisHotOutcome::Failed));
    assert!(manager.scheduler_redis_degraded_until.lock().is_some());
}

#[tokio::test]
async fn confirmed_concurrent_admission_success_clears_error_streak() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let manager = Arc::new(
        MultiTokenManager::new_with_stores(
            Config::default(),
            vec![test_access_token_credential("token-1", "Pro")],
            None,
            None,
            false,
            None,
            Some(redis_store),
        )
        .unwrap(),
    );
    let slow_manager = manager.clone();
    let slow_success = tokio::spawn(async move {
        slow_manager
            .await_scheduler_redis_admission_outcome("并发 Redis 准入成功", true, async move {
                tokio::time::sleep(SCHEDULER_REDIS_HOT_OP_TIMEOUT + StdDuration::from_millis(25))
                    .await;
                Ok::<_, anyhow::Error>(11_u32)
            })
            .await
    });
    tokio::time::sleep(StdDuration::from_millis(10)).await;

    let failed = manager
        .await_scheduler_redis_admission_outcome("Redis 准入明确错误", true, async move {
            Err::<u32, _>(anyhow::anyhow!("redis connection closed"))
        })
        .await;
    assert!(matches!(failed, SchedulerRedisHotOutcome::Failed));
    assert!(manager.scheduler_redis_degraded_until.lock().is_none());
    assert_eq!(
        manager
            .scheduler_redis_admission_failure_streak
            .load(Ordering::Acquire),
        1
    );

    assert!(matches!(
        slow_success.await.unwrap(),
        SchedulerRedisHotOutcome::Completed(11)
    ));
    assert!(manager.scheduler_redis_degraded_until.lock().is_none());
    assert_eq!(
        manager
            .scheduler_redis_admission_failure_streak
            .load(Ordering::Acquire),
        0
    );
}

#[test]
fn test_scheduler_redis_degraded_backoff_grows_on_repeated_failures() {
    let manager = MultiTokenManager::new(
        Config::default(),
        vec![test_access_token_credential("token-1", "Pro")],
        None,
        None,
        false,
    )
    .unwrap();
    let err = anyhow::anyhow!("redis timeout");

    manager.mark_scheduler_redis_admission_degraded("测试 Redis 准入热路径", &err);
    let first_deadline = *manager.scheduler_redis_degraded_until.lock();
    let first_remaining = first_deadline
        .expect("degraded deadline")
        .saturating_duration_since(Instant::now());

    *manager.scheduler_redis_degraded_until.lock() = None;
    manager.mark_scheduler_redis_admission_degraded("测试 Redis 准入热路径", &err);
    let second_deadline = *manager.scheduler_redis_degraded_until.lock();
    let second_remaining = second_deadline
        .expect("degraded deadline")
        .saturating_duration_since(Instant::now());

    assert!(first_remaining <= SCHEDULER_REDIS_DEGRADED_BACKOFF_BASE);
    assert!(second_remaining > first_remaining);
    assert!(second_remaining <= SCHEDULER_REDIS_DEGRADED_BACKOFF_MAX);
}

fn test_access_token_credential(token: &str, subscription_title: &str) -> KiroCredentials {
    let mut credential = KiroCredentials::default();
    credential.subscription_title = Some(subscription_title.to_string());
    credential.access_token = Some(token.to_string());
    credential.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    credential
}

#[test]
fn test_is_token_expired_with_expired_token() {
    let mut credentials = KiroCredentials::default();
    credentials.expires_at = Some("2020-01-01T00:00:00Z".to_string());
    assert!(is_token_expired(&credentials));
}

#[test]
fn test_is_token_expired_with_valid_token() {
    let mut credentials = KiroCredentials::default();
    let future = Utc::now() + Duration::hours(1);
    credentials.expires_at = Some(future.to_rfc3339());
    assert!(!is_token_expired(&credentials));
}

#[test]
fn test_is_token_expired_within_5_minutes() {
    let mut credentials = KiroCredentials::default();
    let expires = Utc::now() + Duration::minutes(3);
    credentials.expires_at = Some(expires.to_rfc3339());
    assert!(is_token_expired(&credentials));
}

#[test]
fn test_is_token_expired_no_expires_at() {
    let credentials = KiroCredentials::default();
    assert!(is_token_expired(&credentials));
}

#[test]
fn test_is_token_expiring_soon_within_10_minutes() {
    let mut credentials = KiroCredentials::default();
    let expires = Utc::now() + Duration::minutes(8);
    credentials.expires_at = Some(expires.to_rfc3339());
    assert!(is_token_expiring_soon(&credentials));
}

#[test]
fn test_is_token_expiring_soon_beyond_10_minutes() {
    let mut credentials = KiroCredentials::default();
    let expires = Utc::now() + Duration::minutes(15);
    credentials.expires_at = Some(expires.to_rfc3339());
    assert!(!is_token_expiring_soon(&credentials));
}

#[test]
fn test_validate_refresh_token_missing() {
    let credentials = KiroCredentials::default();
    let result = validate_refresh_token(&credentials);
    assert!(result.is_err());
}

#[test]
fn test_validate_refresh_token_valid() {
    let mut credentials = KiroCredentials::default();
    credentials.refresh_token = Some("a".repeat(150));
    let result = validate_refresh_token(&credentials);
    assert!(result.is_ok());
}

#[test]
fn test_invalid_grant_resource_not_found_is_permanent_refresh_failure() {
    assert!(is_invalid_grant_response(
        reqwest::StatusCode::BAD_REQUEST,
        r#"{"error":"invalid_grant","error_description":"Resource not found"}"#
    ));
    assert!(!is_invalid_grant_response(
        reqwest::StatusCode::UNAUTHORIZED,
        r#"{"error":"invalid_grant","error_description":"Resource not found"}"#
    ));
    assert!(!is_invalid_grant_response(
        reqwest::StatusCode::BAD_REQUEST,
        r#"{"error":"slow_down"}"#
    ));
}

#[test]
fn test_sha256_hex() {
    let result = sha256_hex("test");
    assert_eq!(
        result,
        "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
    );
}

#[test]
fn usage_limits_user_agents_match_kiro_rest_shape() {
    assert_eq!(
        usage_limits_amz_user_agent("0.12.155", "machine"),
        "aws-sdk-js/1.0.0 KiroIDE-0.12.155-machine"
    );
    assert_eq!(
        usage_limits_user_agent("macos#23.4.0", "22.22.0", "0.12.155", "machine"),
        "aws-sdk-js/1.0.0 ua/2.1 os/macos#23.4.0 lang/js md/nodejs#22.22.0 api/codewhispererruntime#1.0.0 m/N,E KiroIDE-0.12.155-machine"
    );
}

#[tokio::test]
async fn test_refresh_token_rejects_api_key_credential() {
    let config = Config::default();
    let mut credentials = KiroCredentials::default();
    credentials.kiro_api_key = Some("ksk_test_key_123".to_string());
    credentials.auth_method = Some("api_key".to_string());

    let result = refresh_token(&credentials, &config, None).await;

    assert!(result.is_err(), "API Key 凭据应被 refresh_token 拒绝");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("API Key 凭据不支持刷新"),
        "期望错误消息包含 'API Key 凭据不支持刷新'，实际: {}",
        err_msg
    );
}

#[tokio::test]
async fn test_add_credential_reject_duplicate_refresh_token() {
    let config = Config::default();

    let mut existing = KiroCredentials::default();
    existing.refresh_token = Some("a".repeat(150));

    let manager = MultiTokenManager::new(config, vec![existing], None, None, false).unwrap();

    let mut duplicate = KiroCredentials::default();
    duplicate.refresh_token = Some("a".repeat(150));

    let result = manager.add_credential(duplicate).await;
    assert!(result.is_err());
    assert!(result.err().unwrap().to_string().contains("凭据已存在"));
}

#[tokio::test]
async fn test_add_credential_api_key_success() {
    let config = Config::default();
    let manager = MultiTokenManager::new(config, vec![], None, None, false).unwrap();

    let mut api_key_cred = KiroCredentials::default();
    api_key_cred.kiro_api_key = Some("ksk_test_key_123".to_string());
    api_key_cred.auth_method = Some("api_key".to_string());

    let result = manager.add_credential(api_key_cred).await;
    assert!(result.is_ok());
    let id = result.unwrap();
    assert!(id > 0);
    assert_eq!(manager.total_count(), 1);
    assert_eq!(manager.available_count(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_row_level_update_does_not_delete_credentials_added_by_other_instance() {
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };

    let mut first = api_key_credential("ksk_first_row_level");
    first.id = Some(1);
    first.priority = 1;
    store.save_credentials(&[first.clone()]).await.unwrap();

    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![first],
        None,
        None,
        false,
        Some(store.clone()),
        None,
    )
    .unwrap();

    let second = store
        .insert_credential(&KiroCredentials {
            kiro_api_key: Some("ksk_second_other_instance".to_string()),
            auth_method: Some("api_key".to_string()),
            priority: 2,
            ..Default::default()
        })
        .await
        .unwrap();

    manager.set_priority(1, 5).unwrap();
    let loaded = store.load_credentials().await.unwrap();
    assert!(
        loaded
            .iter()
            .any(|credential| credential.id == Some(1) && credential.priority == 5),
        "当前实例更新的凭据应被行级保存"
    );
    assert!(
        loaded.iter().any(|credential| credential.id == second.id),
        "其他实例新增的凭据不应被旧内存快照软删除"
    );

    store.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_failure_counts_are_atomic_across_managers() {
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };

    let mut credential = api_key_credential("ksk_atomic_failure_count");
    credential.id = Some(1);
    store.save_credentials(&[credential.clone()]).await.unwrap();

    let manager_a = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![credential.clone()],
        None,
        None,
        false,
        Some(store.clone()),
        None,
    )
    .unwrap();
    let manager_b = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![credential],
        None,
        None,
        false,
        Some(store.clone()),
        None,
    )
    .unwrap();

    assert!(manager_a.report_failure(1));
    assert!(manager_b.report_failure(1));
    assert!(!manager_a.report_failure(1));

    let runtime_state = store.load_credential_runtime_state().await.unwrap();
    let state = runtime_state.get(&1).unwrap();
    assert_eq!(state.failure_count, MAX_FAILURES_PER_CREDENTIAL);
    assert_eq!(
        state.disabled_reason.as_deref(),
        Some(DisabledReason::TooManyFailures.as_str())
    );
    let credentials = store.load_credentials().await.unwrap();
    assert!(
        credentials
            .iter()
            .any(|credential| { credential.id == Some(1) && credential.disabled })
    );
    let snapshot = manager_a.snapshot();
    assert!(snapshot.entries.iter().any(|entry| {
        entry.id == 1 && entry.disabled && entry.failure_count == MAX_FAILURES_PER_CREDENTIAL
    }));

    store.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_success_reset_is_ordered_before_next_cross_manager_failure() {
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };

    let mut credential = api_key_credential("ksk_atomic_success_reset");
    credential.id = Some(1);
    store.save_credentials(&[credential.clone()]).await.unwrap();
    let manager_a = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![credential.clone()],
        None,
        None,
        false,
        Some(store.clone()),
        None,
    )
    .unwrap();
    let manager_b = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![credential],
        None,
        None,
        false,
        Some(store.clone()),
        None,
    )
    .unwrap();

    assert!(manager_a.report_failure(1));
    assert!(manager_a.report_failure(1));
    manager_b.report_success(1);
    assert!(manager_a.report_failure(1));

    let runtime_state = store.load_credential_runtime_state().await.unwrap();
    let state = runtime_state.get(&1).unwrap();
    assert_eq!(state.failure_count, 1);
    assert_eq!(state.refresh_failure_count, 0);
    assert!(state.disabled_reason.is_none());
    assert!(
        store
            .load_credentials()
            .await
            .unwrap()
            .iter()
            .any(|credential| credential.id == Some(1) && !credential.disabled)
    );
    assert!(
        manager_a
            .snapshot()
            .entries
            .iter()
            .any(|entry| { entry.id == 1 && !entry.disabled && entry.failure_count == 1 })
    );

    store.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_success_auto_reenables_cross_manager_failure_disable_after_postgres_recovers() {
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };

    let mut credential = api_key_credential("ksk_queued_success_auto_reenable");
    credential.id = Some(1);
    store.save_credentials(&[credential.clone()]).await.unwrap();
    let manager_a = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![credential.clone()],
        None,
        None,
        false,
        Some(store.clone()),
        None,
    )
    .unwrap();
    let manager_b = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![credential],
        None,
        None,
        false,
        Some(store.clone()),
        None,
    )
    .unwrap();

    assert!(manager_a.report_failure(1));
    assert!(manager_a.report_failure(1));
    let first_connection = store.pool().acquire().await.unwrap();
    let second_connection = store.pool().acquire().await.unwrap();
    manager_b.report_success(1);
    assert_eq!(manager_b.runtime_mutation_backlog().0, 1);

    drop(first_connection);
    drop(second_connection);
    assert!(!manager_a.report_failure(1));
    assert!(
        store
            .load_credentials()
            .await
            .unwrap()
            .iter()
            .any(|credential| credential.id == Some(1) && credential.disabled)
    );

    manager_b.flush_pending_runtime_mutations_with_budget(StdDuration::from_secs(2));
    assert_eq!(manager_b.runtime_mutation_backlog(), (0, 0));
    let runtime = store.load_credential_runtime_state().await.unwrap();
    assert_eq!(runtime[&1].failure_count, 0);
    assert!(runtime[&1].disabled_reason.is_none());
    assert!(
        store
            .load_credentials()
            .await
            .unwrap()
            .iter()
            .any(|credential| credential.id == Some(1) && !credential.disabled)
    );
    assert!(manager_a.reload_credentials_from_postgres().unwrap());
    let reloaded = &manager_a.snapshot().entries[0];
    assert!(!reloaded.disabled);
    assert!(reloaded.disabled_reason.is_none());

    store.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_refresh_failure_counts_are_atomic_across_managers() {
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };

    let mut credential = api_key_credential("ksk_atomic_refresh_failure_count");
    credential.id = Some(1);
    store.save_credentials(&[credential.clone()]).await.unwrap();
    let manager_a = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![credential.clone()],
        None,
        None,
        false,
        Some(store.clone()),
        None,
    )
    .unwrap();
    let manager_b = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![credential],
        None,
        None,
        false,
        Some(store.clone()),
        None,
    )
    .unwrap();

    assert!(manager_a.report_refresh_failure(1));
    assert!(manager_b.report_refresh_failure(1));
    assert!(!manager_a.report_refresh_failure(1));

    let runtime_state = store.load_credential_runtime_state().await.unwrap();
    let state = runtime_state.get(&1).unwrap();
    assert_eq!(state.refresh_failure_count, MAX_FAILURES_PER_CREDENTIAL);
    assert_eq!(
        state.disabled_reason.as_deref(),
        Some(DisabledReason::TooManyRefreshFailures.as_str())
    );
    assert!(
        store
            .load_credentials()
            .await
            .unwrap()
            .iter()
            .any(|credential| credential.id == Some(1) && credential.disabled)
    );

    store.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_auto_heal_reenables_credential_for_every_manager() {
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };

    let mut credential = api_key_credential("ksk_atomic_auto_heal");
    credential.id = Some(1);
    store.save_credentials(&[credential.clone()]).await.unwrap();
    let manager_a = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![credential.clone()],
        None,
        None,
        false,
        Some(store.clone()),
        None,
    )
    .unwrap();
    let manager_b = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![credential],
        None,
        None,
        false,
        Some(store.clone()),
        None,
    )
    .unwrap();

    assert!(manager_a.report_failure(1));
    assert!(manager_a.report_failure(1));
    assert!(!manager_a.report_failure(1));
    assert!(manager_b.reload_credentials_from_postgres().unwrap());
    assert!(manager_b.auto_heal_too_many_failures_if_applicable());
    assert!(manager_a.reload_credentials_from_postgres().unwrap());

    for manager in [&manager_a, &manager_b] {
        let snapshot = manager.snapshot();
        let entry = snapshot.entries.iter().find(|entry| entry.id == 1).unwrap();
        assert!(!entry.disabled);
        assert_eq!(entry.failure_count, 0);
        assert!(entry.disabled_reason.is_none());
    }
    let stored = store.load_credentials().await.unwrap();
    assert!(
        stored
            .iter()
            .any(|credential| credential.id == Some(1) && !credential.disabled)
    );
    let runtime_state = store.load_credential_runtime_state().await.unwrap();
    assert!(runtime_state.get(&1).unwrap().disabled_reason.is_none());

    store.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn postgres_pending_runtime_mutations_replay_in_order_and_unquarantine() {
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };

    let mut credential = api_key_credential("ksk_pending_runtime_replay");
    credential.id = Some(1);
    store.save_credentials(&[credential.clone()]).await.unwrap();
    let initial = store
        .record_credential_api_failure(
            1,
            uuid::Uuid::new_v4(),
            &Utc::now().to_rfc3339(),
            MAX_FAILURES_PER_CREDENTIAL,
        )
        .await
        .unwrap();
    assert_eq!(initial.failure_count, 1);
    assert_eq!(initial.revision, 1);

    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![credential],
        None,
        None,
        false,
        Some(store.clone()),
        None,
    )
    .unwrap();
    {
        let mut entries = manager.entries.lock();
        assert_eq!(entries[0].runtime_revision, 1);
        entries[0].failure_count = 0;
    }
    assert!(manager.enqueue_pending_runtime_mutation(
        1,
        PendingCredentialRuntimeMutation::Success {
            operation_id: uuid::Uuid::new_v4(),
            expected_generation: 0,
            count: 1,
        },
    ));
    {
        let mut entries = manager.entries.lock();
        entries[0].failure_count = 1;
    }
    assert!(manager.enqueue_pending_runtime_mutation(
        1,
        PendingCredentialRuntimeMutation::ApiFailure {
            operation_id: uuid::Uuid::new_v4(),
            expected_generation: 0,
            last_used_at: Utc::now().to_rfc3339(),
        },
    ));

    assert!(!manager.load_runtime_state());
    {
        let entries = manager.entries.lock();
        assert!(entries[0].runtime_persistence_degraded);
        assert!(entries[0].disabled);
        assert_eq!(entries[0].failure_count, 1);
        assert_eq!(entries[0].runtime_revision, 1);
    }
    assert_eq!(manager.runtime_mutation_backlog().0, 2);

    let mut failed_ids = HashSet::new();
    assert!(manager.flush_pending_runtime_mutations_until(
        Instant::now() + StdDuration::from_secs(2),
        1,
        &mut failed_ids,
    ));
    assert_eq!(manager.runtime_mutation_backlog().0, 1);
    {
        let entries = manager.entries.lock();
        assert!(entries[0].runtime_persistence_degraded);
        assert!(entries[0].disabled);
    }

    manager.flush_pending_runtime_mutations();

    assert_eq!(manager.runtime_mutation_backlog(), (0, 0));
    {
        let entries = manager.entries.lock();
        assert!(!entries[0].runtime_persistence_degraded);
        assert!(!entries[0].disabled);
        assert_eq!(entries[0].failure_count, 1);
        assert_eq!(entries[0].runtime_revision, 3);
    }
    let states = store.load_credential_runtime_state().await.unwrap();
    assert_eq!(states[&1].failure_count, 1);
    assert_eq!(states[&1].revision, 3);

    crate::kiro::token_manager::drain_best_effort_storage_tasks(StdDuration::from_secs(1)).await;
    store.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn postgres_reset_generation_fences_pending_failure_and_disable_replay() {
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };

    let mut credential = api_key_credential("ksk_pending_runtime_generation_fence");
    credential.id = Some(1);
    store.save_credentials(&[credential.clone()]).await.unwrap();
    let manager_a = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![credential.clone()],
        None,
        None,
        false,
        Some(store.clone()),
        None,
    )
    .unwrap();
    let manager_b = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![credential],
        None,
        None,
        false,
        Some(store.clone()),
        None,
    )
    .unwrap();

    assert!(manager_a.enqueue_pending_runtime_mutation(
        1,
        PendingCredentialRuntimeMutation::ApiFailure {
            operation_id: uuid::Uuid::new_v4(),
            expected_generation: 0,
            last_used_at: Utc::now().to_rfc3339(),
        },
    ));
    assert!(manager_a.enqueue_pending_runtime_mutation(
        1,
        PendingCredentialRuntimeMutation::Disable {
            operation_id: uuid::Uuid::new_v4(),
            expected_generation: 0,
            reason: DisabledReason::TooManyFailures.as_str().to_string(),
            failure_count: Some(MAX_FAILURES_PER_CREDENTIAL),
            refresh_failure_count: None,
            last_used_at: Utc::now().to_rfc3339(),
        },
    ));
    assert_eq!(manager_a.runtime_mutation_backlog().0, 2);

    manager_b.reset_and_enable(1).unwrap();
    manager_a.flush_pending_runtime_mutations();

    assert_eq!(manager_a.runtime_mutation_backlog(), (0, 0));
    {
        let entries = manager_a.entries.lock();
        let entry = entries.iter().find(|entry| entry.id == 1).unwrap();
        assert_eq!(entry.runtime_generation, 1);
        assert_eq!(entry.failure_count, 0);
        assert_eq!(entry.refresh_failure_count, 0);
        assert!(!entry.runtime_persistence_degraded);
        assert!(!entry.disabled);
        assert!(entry.disabled_reason.is_none());
    }
    let states = store.load_credential_runtime_state().await.unwrap();
    let reset_state = states.get(&1).unwrap();
    assert_eq!(reset_state.generation, 1);
    assert_eq!(reset_state.failure_count, 0);
    assert_eq!(reset_state.refresh_failure_count, 0);
    assert!(reset_state.disabled_reason.is_none());
    assert!(
        store
            .load_credentials()
            .await
            .unwrap()
            .iter()
            .any(|credential| credential.id == Some(1) && !credential.disabled)
    );

    assert!(manager_a.report_failure(1));
    let states = store.load_credential_runtime_state().await.unwrap();
    let current = states.get(&1).unwrap();
    assert_eq!(current.generation, 1);
    assert_eq!(current.failure_count, 1);
    assert!(current.disabled_reason.is_none());

    crate::kiro::token_manager::drain_best_effort_storage_tasks(StdDuration::from_secs(1)).await;
    store.drop_test_schema().await.unwrap();
}

#[tokio::test]
async fn test_add_credential_reject_duplicate_api_key() {
    let config = Config::default();

    let mut existing = KiroCredentials::default();
    existing.kiro_api_key = Some("ksk_existing_key".to_string());
    existing.auth_method = Some("api_key".to_string());

    let manager = MultiTokenManager::new(config, vec![existing], None, None, false).unwrap();

    let mut duplicate = KiroCredentials::default();
    duplicate.kiro_api_key = Some("ksk_existing_key".to_string());
    duplicate.auth_method = Some("api_key".to_string());

    let result = manager.add_credential(duplicate).await;
    assert!(result.is_err());
    assert!(
        result
            .err()
            .unwrap()
            .to_string()
            .contains("kiroApiKey 重复")
    );
}

#[tokio::test]
async fn test_add_credential_api_key_empty_rejected() {
    let config = Config::default();
    let manager = MultiTokenManager::new(config, vec![], None, None, false).unwrap();

    let mut cred = KiroCredentials::default();
    cred.kiro_api_key = Some(String::new());
    cred.auth_method = Some("api_key".to_string());

    let result = manager.add_credential(cred).await;
    assert!(result.is_err());
    assert!(
        result
            .err()
            .unwrap()
            .to_string()
            .contains("kiroApiKey 为空")
    );
}

#[tokio::test]
async fn test_add_credential_api_key_missing_key_rejected() {
    let config = Config::default();
    let manager = MultiTokenManager::new(config, vec![], None, None, false).unwrap();

    let mut cred = KiroCredentials::default();
    cred.auth_method = Some("api_key".to_string());
    // kiro_api_key is None

    let result = manager.add_credential(cred).await;
    assert!(result.is_err());
    assert!(
        result
            .err()
            .unwrap()
            .to_string()
            .contains("缺少 kiroApiKey")
    );
}

#[tokio::test]
async fn test_add_credential_api_key_and_oauth_coexist() {
    let config = Config::default();

    let mut oauth_cred = KiroCredentials::default();
    oauth_cred.refresh_token = Some("a".repeat(150));

    let manager = MultiTokenManager::new(config, vec![oauth_cred], None, None, false).unwrap();

    let mut api_key_cred = KiroCredentials::default();
    api_key_cred.kiro_api_key = Some("ksk_new_key".to_string());
    api_key_cred.auth_method = Some("api_key".to_string());

    let result = manager.add_credential(api_key_cred).await;
    assert!(result.is_ok());
    assert_eq!(manager.total_count(), 2);
    assert_eq!(manager.available_count(), 2);
}

// MultiTokenManager 测试

#[test]
fn test_multi_token_manager_new() {
    let config = Config::default();
    let mut cred1 = KiroCredentials::default();
    cred1.priority = 0;
    let mut cred2 = KiroCredentials::default();
    cred2.priority = 1;

    let manager = MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();
    assert_eq!(manager.total_count(), 2);
    assert_eq!(manager.available_count(), 2);
}

#[test]
fn test_multi_token_manager_empty_credentials() {
    let config = Config::default();
    let result = MultiTokenManager::new(config, vec![], None, None, false);
    // 支持 0 个凭据启动（可通过管理面板添加）
    assert!(result.is_ok());
    let manager = result.unwrap();
    assert_eq!(manager.total_count(), 0);
    assert_eq!(manager.available_count(), 0);
}

#[test]
fn test_multi_token_manager_duplicate_ids() {
    let config = Config::default();
    let mut cred1 = KiroCredentials::default();
    cred1.id = Some(1);
    let mut cred2 = KiroCredentials::default();
    cred2.id = Some(1); // 重复 ID

    let result = MultiTokenManager::new(config, vec![cred1, cred2], None, None, false);
    assert!(result.is_err());
    let err_msg = result.err().unwrap().to_string();
    assert!(
        err_msg.contains("重复的凭据 ID"),
        "错误消息应包含 '重复的凭据 ID'，实际: {}",
        err_msg
    );
}

#[test]
fn test_multi_token_manager_api_key_missing_kiro_api_key_auto_disabled() {
    let config = Config::default();

    // auth_method=api_key 但缺少 kiro_api_key → 应被自动禁用
    let mut bad_cred = KiroCredentials::default();
    bad_cred.auth_method = Some("api_key".to_string());
    // kiro_api_key 保持 None

    let mut good_cred = KiroCredentials::default();
    good_cred.refresh_token = Some("valid_token".to_string());

    let manager =
        MultiTokenManager::new(config, vec![bad_cred, good_cred], None, None, false).unwrap();
    assert_eq!(manager.total_count(), 2);
    assert_eq!(manager.available_count(), 1); // bad_cred 被禁用，只剩 1 个可用
}

#[test]
fn test_multi_token_manager_api_key_with_kiro_api_key_not_disabled() {
    let config = Config::default();

    // auth_method=api_key 且有 kiro_api_key → 不应被禁用
    let mut cred = KiroCredentials::default();
    cred.auth_method = Some("api_key".to_string());
    cred.kiro_api_key = Some("ksk_test123".to_string());

    let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();
    assert_eq!(manager.total_count(), 1);
    assert_eq!(manager.available_count(), 1);
}

#[test]
fn test_multi_token_manager_report_failure() {
    let config = Config::default();
    let cred1 = KiroCredentials::default();
    let cred2 = KiroCredentials::default();

    let manager = MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

    // 凭据会自动分配 ID（从 1 开始）
    // 前两次失败不会禁用（使用 ID 1）
    assert!(manager.report_failure(1));
    assert!(manager.report_failure(1));
    assert_eq!(manager.available_count(), 2);

    // 第三次失败会禁用第一个凭据
    assert!(manager.report_failure(1));
    assert_eq!(manager.available_count(), 1);

    // 继续失败第二个凭据（使用 ID 2）
    assert!(manager.report_failure(2));
    assert!(manager.report_failure(2));
    assert!(!manager.report_failure(2)); // 所有凭据都禁用了
    assert_eq!(manager.available_count(), 0);
}

#[test]
fn test_multi_token_manager_report_success() {
    let config = Config::default();
    let cred = KiroCredentials::default();

    let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();

    // 失败两次（使用 ID 1）
    manager.report_failure(1);
    manager.report_failure(1);

    // 成功后重置计数（使用 ID 1）
    manager.report_success(1);

    // 再失败两次不会禁用
    manager.report_failure(1);
    manager.report_failure(1);
    assert_eq!(manager.available_count(), 1);
}

#[test]
fn test_multi_token_manager_switch_to_next() {
    let config = Config::default();
    let mut cred1 = KiroCredentials::default();
    cred1.refresh_token = Some("token1".to_string());
    let mut cred2 = KiroCredentials::default();
    cred2.refresh_token = Some("token2".to_string());

    let manager = MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

    let initial_id = manager.snapshot().current_id;

    // 切换到下一个
    assert!(manager.switch_to_next());
    assert_ne!(manager.snapshot().current_id, initial_id);
}

#[test]
fn test_set_load_balancing_mode_updates_runtime_memory_without_store() {
    let config = Config::default();
    let manager =
        MultiTokenManager::new(config, vec![KiroCredentials::default()], None, None, false)
            .unwrap();

    manager
        .set_load_balancing_mode("balanced".to_string())
        .unwrap();

    assert_eq!(manager.get_load_balancing_mode(), "balanced");
    assert_eq!(manager.runtime_config().load_balancing_mode, "balanced");
}

#[test]
fn test_update_runtime_config_updates_runtime_memory_without_store() {
    use crate::model::config::{
        ReportedUsageConfig, ReportedUsageFieldPolicy, ReportedUsagePathPolicy,
    };

    let config = Config::default();
    let manager =
        MultiTokenManager::new(config, vec![KiroCredentials::default()], None, None, false)
            .unwrap();

    manager
        .update_runtime_config(|config| {
            config.credential_dispatch_max_wait_secs = 77;
            config.reported_usage = ReportedUsageConfig {
                default: ReportedUsagePathPolicy::default(),
                path_overrides: [(
                    "/custom".to_string(),
                    ReportedUsagePathPolicy {
                        input: ReportedUsageFieldPolicy::sample_input_max(42),
                        ..ReportedUsagePathPolicy::default()
                    },
                )]
                .into_iter()
                .collect(),
            };
        })
        .unwrap();

    assert_eq!(
        manager
            .runtime_config()
            .reported_usage
            .policy_for_path("/custom/v1/messages")
            .input
            .max_tokens,
        42
    );
}

#[tokio::test]
async fn test_multi_token_manager_acquire_context_auto_recovers_all_disabled() {
    let config = Config::default();
    let mut cred1 = KiroCredentials::default();
    cred1.access_token = Some("t1".to_string());
    cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    let mut cred2 = KiroCredentials::default();
    cred2.access_token = Some("t2".to_string());
    cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager = MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

    // 凭据会自动分配 ID（从 1 开始）
    for _ in 0..MAX_FAILURES_PER_CREDENTIAL {
        manager.report_failure(1);
    }
    for _ in 0..MAX_FAILURES_PER_CREDENTIAL {
        manager.report_failure(2);
    }

    assert_eq!(manager.available_count(), 0);

    // 应触发自愈：重置失败计数并重新启用，避免必须重启进程
    let mut ctx = manager.acquire_context(None).await.unwrap();
    assert!(ctx.token == "t1" || ctx.token == "t2");
    assert_eq!(manager.available_count(), 2);
    ctx.release_in_flight();
}

#[tokio::test]
async fn test_multi_token_manager_acquire_context_balanced_retries_until_bad_credential_disabled() {
    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();

    let mut bad_cred = KiroCredentials::default();
    bad_cred.priority = 0;
    bad_cred.refresh_token = Some("bad".to_string());

    let mut good_cred = KiroCredentials::default();
    good_cred.priority = 1;
    good_cred.access_token = Some("good-token".to_string());
    good_cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager =
        MultiTokenManager::new(config, vec![bad_cred, good_cred], None, None, false).unwrap();

    let mut ctx = manager.acquire_context(None).await.unwrap();
    assert_eq!(ctx.id, 2);
    assert_eq!(ctx.token, "good-token");
    ctx.release_in_flight();
}

#[tokio::test]
async fn test_all_bad_refresh_tokens_are_bounded_by_auth_cooldown() {
    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();

    let mut first = KiroCredentials::default();
    first.refresh_token = Some("bad".to_string());
    let mut second = KiroCredentials::default();
    second.refresh_token = Some("also-bad".to_string());

    let manager = MultiTokenManager::new(config, vec![first, second], None, None, false).unwrap();
    let started = Instant::now();
    let err = manager
        .acquire_context(None)
        .await
        .err()
        .unwrap()
        .to_string();

    assert!(
        started.elapsed() < StdDuration::from_millis(500),
        "全部 refreshToken 无效时应按凭据数量和失败阈值有界结束，不应持续打"
    );
    assert!(
        err.contains("所有可用账号均处于上游临时冷却"),
        "错误应明确结束调度并要求退避，实际: {}",
        err
    );
    let snapshot = manager.snapshot();
    assert_eq!(snapshot.available, 2);
    assert!(snapshot.entries.iter().all(|entry| !entry.disabled));
    assert!(
        snapshot
            .entries
            .iter()
            .all(|entry| entry.refresh_failure_count == 0)
    );
    assert!(snapshot.entries.iter().all(|entry| entry.cooled_down));
}

#[tokio::test]
async fn test_acquire_context_sticks_same_session_to_same_credential_in_balanced_mode() {
    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();

    let mut cred1 = KiroCredentials::default();
    cred1.access_token = Some("t1".to_string());
    cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    let mut cred2 = KiroCredentials::default();
    cred2.access_token = Some("t2".to_string());
    cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager = MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();
    let excluded = HashSet::new();

    let first = manager
        .acquire_context_for_session(None, Some("session-a"), &excluded)
        .await
        .unwrap();
    manager.report_success_for_session(first.id, Some("session-a"));

    let second = manager
        .acquire_context_for_session(None, Some("session-a"), &excluded)
        .await
        .unwrap();

    assert_eq!(first.id, second.id);
}

#[tokio::test]
async fn test_model_specific_cooldown_only_blocks_same_model() {
    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();

    let mut cred1 = KiroCredentials::default();
    cred1.access_token = Some("t1".to_string());
    cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    let mut cred2 = KiroCredentials::default();
    cred2.access_token = Some("t2".to_string());
    cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager = MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();
    manager
        .report_transient_failure_kind(
            1,
            Some("claude-opus-4.8"),
            TransientFailureKind::RateLimit,
            Some(StdDuration::from_secs(60)),
            "429",
        )
        .unwrap();

    let mut sonnet = manager
        .acquire_context_for_session(Some("claude-sonnet-4.6"), None, &HashSet::new())
        .await
        .unwrap();
    assert_eq!(sonnet.id, 1);
    sonnet.release_in_flight();

    let mut opus = manager
        .acquire_context_for_session(Some("claude-opus-4.8"), None, &HashSet::new())
        .await
        .unwrap();
    assert_eq!(opus.id, 2);
    opus.release_in_flight();

    let snapshot = manager.snapshot();
    let entry = snapshot.entries.iter().find(|entry| entry.id == 1).unwrap();
    assert!(entry.cooled_down);
    assert_eq!(entry.cooldowns.len(), 1);
    assert_eq!(entry.cooldowns[0].model.as_deref(), Some("claude-opus-4.8"));
}

#[tokio::test]
async fn test_supported_model_exact_match_allows_local_scheduler_selection() {
    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();

    let mut cred = KiroCredentials::default();
    cred.access_token = Some("t1".to_string());
    cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    cred.supported_models = vec!["claude-sonnet-4".to_string()];

    let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();
    let mut ctx = manager
        .acquire_context_for_session(Some("claude-sonnet-4"), None, &HashSet::new())
        .await
        .unwrap();

    assert_eq!(ctx.id, 1);
    ctx.release_in_flight();
}

#[tokio::test]
async fn test_supported_model_filter_does_not_alias_local_scheduler_selection() {
    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();

    let mut cred = KiroCredentials::default();
    cred.access_token = Some("t1".to_string());
    cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    cred.supported_models = vec!["claude-sonnet-4-20250514".to_string()];

    let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();
    let err = manager
        .acquire_context_for_session(Some("claude-sonnet-4"), None, &HashSet::new())
        .await
        .err()
        .unwrap()
        .to_string();

    assert!(err.contains("没有支持当前模型的可用账号"), "{err}");
}

#[tokio::test]
async fn test_empty_supported_models_allows_future_model_when_restricted_credential_does_not() {
    let mut config = Config::default();
    config.load_balancing_mode = "priority".to_string();

    let mut restricted = test_access_token_credential("restricted", "Pro");
    restricted.priority = 0;
    restricted.supported_models = vec!["claude-sonnet-4.6".to_string()];

    let mut unrestricted = test_access_token_credential("unrestricted", "Pro");
    unrestricted.priority = 1;
    unrestricted.supported_models = Vec::new();

    let manager =
        MultiTokenManager::new(config, vec![restricted, unrestricted], None, None, false).unwrap();
    let mut ctx = manager
        .acquire_context_for_session(Some("claude-sonnet-5"), None, &HashSet::new())
        .await
        .unwrap();

    assert_eq!(ctx.id, 2);
    ctx.release_in_flight();
}

#[tokio::test]
async fn test_supported_model_alias_does_not_cross_family_in_local_scheduler() {
    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();

    let mut cred = KiroCredentials::default();
    cred.access_token = Some("t1".to_string());
    cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    cred.supported_models = vec!["claude-opus-4.8".to_string()];

    let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();
    let err = manager
        .acquire_context_for_session(Some("claude-sonnet-4.6"), None, &HashSet::new())
        .await
        .err()
        .unwrap()
        .to_string();

    assert!(err.contains("没有支持当前模型的可用账号"), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn test_model_scoped_429_high_concurrency_disabled_and_model_filters() {
    const OPUS_MODEL: &str = "claude-opus-4.8";
    const CREDENTIAL_COUNT: usize = 12;
    const REQUESTS_PER_MODEL: usize = 120;
    const PER_CREDENTIAL_LIMIT: u32 = 2;
    const GLOBAL_LIMIT: u32 = 12;

    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();
    config.credential_max_concurrent_requests = PER_CREDENTIAL_LIMIT;
    config.dispatch_global_max_concurrent_requests = GLOBAL_LIMIT;
    config.dispatch_max_queued_requests = (REQUESTS_PER_MODEL * 2) as u32;
    config.credential_dispatch_max_wait_secs = 5;
    config.credential_rate_limit_cooldown_secs = 30;
    config.credential_max_cooldown_secs = 30;
    config.credential_cooldown_jitter_percent = 0;

    let mut credentials = (1..=CREDENTIAL_COUNT)
        .map(|idx| {
            let subscription = if idx % 2 == 0 { "Pro" } else { "Free" };
            test_access_token_credential(&format!("token-{idx}"), subscription)
        })
        .collect::<Vec<_>>();
    credentials[10].disabled = true;
    credentials[11].disabled = true;

    let manager = Arc::new(MultiTokenManager::new(config, credentials, None, None, false).unwrap());

    for id in [2_u64, 4, 6] {
        manager
            .report_transient_failure_kind(
                id,
                Some(OPUS_MODEL),
                TransientFailureKind::RateLimit,
                Some(StdDuration::from_secs(30)),
                "429 opus high concurrency",
            )
            .unwrap();
    }

    let start = Arc::new(tokio::sync::Barrier::new(REQUESTS_PER_MODEL * 2 + 1));
    let selected_sonnet = Arc::new(std::sync::Mutex::new(Vec::<u64>::new()));
    let selected_opus = Arc::new(std::sync::Mutex::new(Vec::<u64>::new()));
    let mut handles = Vec::with_capacity(REQUESTS_PER_MODEL * 2);

    for idx in 0..REQUESTS_PER_MODEL {
        let manager = manager.clone();
        let start = start.clone();
        let selected_sonnet = selected_sonnet.clone();
        handles.push(tokio::spawn(async move {
            start.wait().await;
            let mut ctx = manager.acquire_context(Some(SONNET_MODEL)).await.unwrap();
            let snapshot = manager.snapshot();
            assert!(
                snapshot.global_in_flight_requests <= GLOBAL_LIMIT,
                "全局并发超限: {} > {}",
                snapshot.global_in_flight_requests,
                GLOBAL_LIMIT
            );
            for entry in &snapshot.entries {
                assert!(
                    entry.in_flight_requests <= entry.max_concurrent_requests,
                    "凭据 #{} 并发超限: {} > {}",
                    entry.id,
                    entry.in_flight_requests,
                    entry.max_concurrent_requests
                );
            }
            tokio::time::sleep(StdDuration::from_millis(2 + (idx % 5) as u64)).await;
            let id = ctx.id;
            manager.report_success_with_latency(id, Some(SONNET_MODEL), None);
            ctx.release_in_flight();
            selected_sonnet.lock().unwrap().push(id);
        }));
    }

    for idx in 0..REQUESTS_PER_MODEL {
        let manager = manager.clone();
        let start = start.clone();
        let selected_opus = selected_opus.clone();
        handles.push(tokio::spawn(async move {
            start.wait().await;
            let mut ctx = manager.acquire_context(Some(OPUS_MODEL)).await.unwrap();
            assert!(
                matches!(ctx.id, 8 | 10),
                "opus 只能调度未冷却且支持 opus 的 Pro 凭据，实际 #{}",
                ctx.id
            );
            tokio::time::sleep(StdDuration::from_millis(2 + (idx % 5) as u64)).await;
            let id = ctx.id;
            manager.report_success_with_latency(id, Some(OPUS_MODEL), None);
            ctx.release_in_flight();
            selected_opus.lock().unwrap().push(id);
        }));
    }

    start.wait().await;
    for handle in handles {
        tokio::time::timeout(StdDuration::from_secs(10), handle)
            .await
            .expect("混合模型高并发调度不应超时")
            .expect("混合模型高并发调度不应 panic");
    }

    let sonnet_ids = selected_sonnet.lock().unwrap().clone();
    let opus_ids = selected_opus.lock().unwrap().clone();
    assert_eq!(sonnet_ids.len(), REQUESTS_PER_MODEL);
    assert_eq!(opus_ids.len(), REQUESTS_PER_MODEL);
    assert!(
        [2_u64, 4, 6].iter().any(|id| sonnet_ids.contains(id)),
        "sonnet 应允许使用仅 opus 模型冷却的凭据，实际分布: {:?}",
        sonnet_ids
    );
    assert!(
        opus_ids.iter().all(|id| matches!(*id, 8 | 10)),
        "opus 不应使用 Free、禁用或 opus 冷却凭据，实际分布: {:?}",
        opus_ids
    );

    let snapshot = manager.snapshot();
    assert_eq!(snapshot.global_in_flight_requests, 0);
    assert_eq!(snapshot.queued_requests, 0);
    for id in [2_u64, 4, 6] {
        let entry = snapshot
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .unwrap();
        assert!(
            entry.cooldowns.iter().any(|cooldown| {
                !cooldown.global && cooldown.model.as_deref() == Some(OPUS_MODEL)
            })
        );
    }
    for id in [11_u64, 12] {
        assert!(
            snapshot
                .entries
                .iter()
                .find(|entry| entry.id == id)
                .unwrap()
                .disabled
        );
    }
}

#[tokio::test]
async fn test_acquire_context_excluded_bound_session_can_fallback() {
    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();

    let mut cred1 = KiroCredentials::default();
    cred1.access_token = Some("t1".to_string());
    cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    let mut cred2 = KiroCredentials::default();
    cred2.access_token = Some("t2".to_string());
    cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager = MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();
    let empty = HashSet::new();

    let first = manager
        .acquire_context_for_session(None, Some("session-b"), &empty)
        .await
        .unwrap();

    let mut excluded = HashSet::new();
    excluded.insert(first.id);
    let fallback = manager
        .acquire_context_for_session(None, Some("session-b"), &excluded)
        .await
        .unwrap();

    assert_ne!(first.id, fallback.id);

    let rebound = manager
        .acquire_context_for_session(None, Some("session-b"), &empty)
        .await
        .unwrap();
    assert_eq!(first.id, rebound.id);
}

#[tokio::test]
async fn test_bound_session_falls_back_when_bound_credential_is_full() {
    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();
    config.credential_max_concurrent_requests = 1;

    let mut cred1 = KiroCredentials::default();
    cred1.access_token = Some("t1".to_string());
    cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    let mut cred2 = KiroCredentials::default();
    cred2.access_token = Some("t2".to_string());
    cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager = MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();
    let empty = HashSet::new();

    let mut bound = manager
        .acquire_context_for_session(None, Some("sticky-full"), &empty)
        .await
        .unwrap();
    manager.report_success_for_session(bound.id, Some("sticky-full"));

    let mut fallback = manager
        .acquire_context_for_session(None, Some("sticky-full"), &empty)
        .await
        .unwrap();

    assert_ne!(
        bound.id, fallback.id,
        "同一 sticky 会话绑定账号并发已满时，应临时调度到其他可用账号，而不是等待绑定账号"
    );
    assert!(fallback.fallback_from_sticky);
    assert!(!fallback.sticky_bound);

    fallback.release_in_flight();
    bound.release_in_flight();

    let rebound = manager
        .acquire_context_for_session(None, Some("sticky-full"), &empty)
        .await
        .unwrap();
    assert_eq!(
        rebound.id, bound.id,
        "并发释放后 sticky 会话应回到原绑定账号，保持粘性"
    );
}

#[tokio::test]
async fn test_transient_failure_cools_down_without_disabling_credential() {
    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();
    config.credential_transient_cooldown_secs = 60;

    let mut cred1 = KiroCredentials::default();
    cred1.access_token = Some("t1".to_string());
    cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    let mut cred2 = KiroCredentials::default();
    cred2.access_token = Some("t2".to_string());
    cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager = MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

    assert!(
        manager
            .report_transient_failure(1, None, Some(StdDuration::from_secs(20)), "429")
            .unwrap()
    );

    let snapshot = manager.snapshot();
    let first = snapshot.entries.iter().find(|entry| entry.id == 1).unwrap();
    assert!(!first.disabled);
    assert_eq!(first.failure_count, 0);
    assert!(first.cooled_down);
    assert!(first.cooldown_remaining_secs > 0);

    let mut ctx = manager.acquire_context(None).await.unwrap();
    assert_eq!(ctx.id, 2);
    assert_eq!(manager.available_count(), 2);
    ctx.release_in_flight();
}

#[tokio::test]
async fn test_transient_failure_does_not_shorten_existing_cooldown() {
    let mut config = Config::default();
    config.credential_transient_cooldown_secs = 60;

    let mut cred1 = KiroCredentials::default();
    cred1.access_token = Some("t1".to_string());
    cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    let mut cred2 = KiroCredentials::default();
    cred2.access_token = Some("t2".to_string());
    cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager = MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

    manager
        .report_transient_failure(1, None, Some(StdDuration::from_secs(30)), "long")
        .unwrap();
    manager
        .report_transient_failure(1, None, Some(StdDuration::from_secs(1)), "short")
        .unwrap();

    let snapshot = manager.snapshot();
    let first = snapshot.entries.iter().find(|entry| entry.id == 1).unwrap();
    assert_eq!(first.cooldown_reason.as_deref(), Some("long"));
    assert!(first.cooldown_remaining_secs >= 20);
}

#[test]
fn test_success_does_not_clear_active_transient_cooldown() {
    let mut config = Config::default();
    config.credential_transient_cooldown_secs = 60;

    let mut cred1 = KiroCredentials::default();
    cred1.access_token = Some("t1".to_string());
    cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    let mut cred2 = KiroCredentials::default();
    cred2.access_token = Some("t2".to_string());
    cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager = MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

    manager
        .report_transient_failure(1, None, Some(StdDuration::from_secs(30)), "429")
        .unwrap();
    manager.report_success(1);

    let snapshot = manager.snapshot();
    let first = snapshot.entries.iter().find(|entry| entry.id == 1).unwrap();
    assert!(first.cooled_down);
    assert_eq!(first.success_count, 1);
}

#[test]
fn test_structured_transient_failure_updates_health_and_backoff() {
    let mut config = Config::default();
    config.credential_rate_limit_cooldown_secs = 1;
    config.credential_max_cooldown_secs = 10;
    config.credential_cooldown_backoff_multiplier = 2.0;
    config.credential_cooldown_jitter_percent = 0;
    let manager =
        MultiTokenManager::new(config, vec![KiroCredentials::default()], None, None, false)
            .unwrap();

    manager
        .report_transient_failure_kind(1, None, TransientFailureKind::RateLimit, None, "429")
        .unwrap();
    {
        let mut entries = manager.entries.lock();
        let entry = entries.iter_mut().find(|entry| entry.id == 1).unwrap();
        let coalesce_window_ms =
            MultiTokenManager::transient_failure_coalesce_window(StdDuration::from_secs(1))
                .as_millis() as i64;
        entry.health.last_error_at_ms =
            Some(Utc::now().timestamp_millis() - coalesce_window_ms - 1);
    }
    manager
        .report_transient_failure_kind(1, None, TransientFailureKind::RateLimit, None, "429 again")
        .unwrap();

    let entry = &manager.snapshot().entries[0];
    assert_eq!(entry.transient_failure_streak, 2);
    assert!(entry.recent_error_rate > 0.0);
    assert_eq!(entry.last_error_kind.as_deref(), Some("rate_limit"));
    assert!(entry.cooldown_remaining_secs >= 2);
    assert!(entry.in_probation);
}

#[test]
fn test_transient_failure_coalesces_same_burst_without_backoff_amplification() {
    let mut config = Config::default();
    config.credential_server_error_cooldown_secs = 5;
    config.credential_max_cooldown_secs = 300;
    config.credential_cooldown_backoff_multiplier = 2.0;
    config.credential_cooldown_jitter_percent = 0;
    let manager = MultiTokenManager::new(
        config,
        vec![test_access_token_credential("token", "Pro")],
        None,
        None,
        false,
    )
    .unwrap();

    for index in 0..12 {
        manager
            .report_transient_failure_kind(
                1,
                None,
                TransientFailureKind::Server,
                None,
                format!("server burst {index}"),
            )
            .unwrap();
    }

    let entry = &manager.snapshot().entries[0];
    assert_eq!(entry.transient_failure_streak, 1);
    assert!(
        entry.cooldown_remaining_secs <= 6,
        "同一波并发 server 错误不应把 5s 基础冷却放大，实际 remaining={}",
        entry.cooldown_remaining_secs
    );
    assert_eq!(entry.last_error_kind.as_deref(), Some("server"));
}

#[test]
fn test_error_specific_cooldown_parameters_are_effective() {
    let cases: [(TransientFailureKind, fn(&mut Config), u64); 6] = [
        (
            TransientFailureKind::RateLimit,
            |config: &mut Config| config.credential_rate_limit_cooldown_secs = 2,
            2,
        ),
        (
            TransientFailureKind::Server,
            |config: &mut Config| config.credential_server_error_cooldown_secs = 3,
            3,
        ),
        (
            TransientFailureKind::Network,
            |config: &mut Config| config.credential_network_error_cooldown_secs = 4,
            4,
        ),
        (
            TransientFailureKind::Stream,
            |config: &mut Config| config.credential_stream_error_cooldown_secs = 5,
            5,
        ),
        (
            TransientFailureKind::Protocol,
            |config: &mut Config| config.credential_protocol_error_cooldown_secs = 6,
            6,
        ),
        (
            TransientFailureKind::Auth,
            |config: &mut Config| config.credential_auth_error_cooldown_secs = 7,
            7,
        ),
    ];

    for (kind, configure, expected_min_secs) in cases {
        let mut config = Config::default();
        config.credential_max_cooldown_secs = 30;
        config.credential_cooldown_backoff_multiplier = 1.0;
        config.credential_cooldown_jitter_percent = 0;
        configure(&mut config);

        let manager = MultiTokenManager::new(
            config,
            vec![test_access_token_credential("token", "Pro")],
            None,
            None,
            false,
        )
        .unwrap();

        manager
            .report_transient_failure_kind(1, None, kind, None, "synthetic")
            .unwrap();
        let entry = &manager.snapshot().entries[0];
        assert_eq!(entry.last_error_kind.as_deref(), Some(kind.as_str()));
        assert!(
            entry.cooldown_remaining_secs >= expected_min_secs,
            "{kind:?} 应使用对应配置冷却，实际 remaining={} expected_min={expected_min_secs}",
            entry.cooldown_remaining_secs
        );
    }
}

#[test]
fn test_scheduler_error_ewma_alpha_changes_error_rate_update() {
    let manager_with_alpha = |alpha: f64| {
        let mut config = Config::default();
        config.scheduler_error_ewma_alpha = alpha;
        config.credential_rate_limit_cooldown_secs = 1;
        config.credential_max_cooldown_secs = 10;
        config.credential_cooldown_jitter_percent = 0;
        MultiTokenManager::new(
            config,
            vec![test_access_token_credential("token", "Pro")],
            None,
            None,
            false,
        )
        .unwrap()
    };

    let low_alpha = manager_with_alpha(0.1);
    let high_alpha = manager_with_alpha(0.9);
    low_alpha
        .report_transient_failure_kind(1, None, TransientFailureKind::RateLimit, None, "429")
        .unwrap();
    high_alpha
        .report_transient_failure_kind(1, None, TransientFailureKind::RateLimit, None, "429")
        .unwrap();

    let low_rate = low_alpha.snapshot().entries[0].recent_error_rate;
    let high_rate = high_alpha.snapshot().entries[0].recent_error_rate;
    assert!(
        high_rate > low_rate,
        "scheduler_error_ewma_alpha 应改变错误率 EWMA 更新幅度，low={low_rate}, high={high_rate}"
    );
    assert!((low_rate - 0.1).abs() < f64::EPSILON);
    assert!((high_rate - 0.9).abs() < f64::EPSILON);
}

#[test]
fn test_health_balanced_score_parameters_are_effective() {
    let mut worse = CredentialEntry {
        id: 1,
        credentials: KiroCredentials {
            priority: 10,
            max_concurrent_requests: Some(2),
            ..Default::default()
        },
        failure_count: 0,
        refresh_failure_count: 0,
        runtime_revision: 0,
        runtime_generation: 0,
        runtime_persistence_degraded: false,
        disabled: false,
        disabled_reason: None,
        success_count: 0,
        total_selection_count: 100,
        last_used_at: None,
        cooldown_until: None,
        cooldown_reason: None,
        model_cooldowns: HashMap::new(),
        rate_limit_available_at: None,
        rate_limit_rpm: None,
        rate_limit_owner_lease_id: None,
        rate_limit_redis_deadline_ms: None,
        pending_redis_admission: None,
        in_flight_requests: 1,
        in_flight_leases: Vec::new(),
        warmup_remaining: 0,
        health: SchedulerHealthState::default(),
        model_health: HashMap::new(),
        selection_events: VecDeque::new(),
    };
    worse.health.recent_error_rate = 0.5;
    worse.health.latency_ewma_ms = Some(1_000.0);
    let now_ms = Utc::now().timestamp_millis();
    worse.health.probation_until_ms = Some(now_ms + 60_000);

    let better = CredentialEntry {
        id: 2,
        credentials: KiroCredentials::default(),
        failure_count: 0,
        refresh_failure_count: 0,
        runtime_revision: 0,
        runtime_generation: 0,
        runtime_persistence_degraded: false,
        disabled: false,
        disabled_reason: None,
        success_count: 0,
        total_selection_count: 0,
        last_used_at: None,
        cooldown_until: None,
        cooldown_reason: None,
        model_cooldowns: HashMap::new(),
        rate_limit_available_at: None,
        rate_limit_rpm: None,
        rate_limit_owner_lease_id: None,
        rate_limit_redis_deadline_ms: None,
        pending_redis_admission: None,
        in_flight_requests: 0,
        in_flight_leases: Vec::new(),
        warmup_remaining: 0,
        health: SchedulerHealthState::default(),
        model_health: HashMap::new(),
        selection_events: VecDeque::new(),
    };

    let mut config = Config::default();
    config.scheduler_priority_weight = 0.0;
    config.scheduler_load_weight = 0.0;
    config.scheduler_error_weight = 0.0;
    config.scheduler_latency_weight = 0.0;
    config.scheduler_probation_weight = 0.0;
    config.scheduler_selection_pressure_weight = 0.0;
    config.scheduler_total_selection_weight = 0.0;

    assert_eq!(
        scheduler_score_with_config(&worse, None, now_ms, 0.0, &config),
        scheduler_score_with_config(&better, None, now_ms, 0.0, &config)
    );

    let weight_setters: [fn(&mut Config); 7] = [
        |config: &mut Config| config.scheduler_priority_weight = 1.0,
        |config: &mut Config| config.scheduler_load_weight = 100.0,
        |config: &mut Config| config.scheduler_error_weight = 100.0,
        |config: &mut Config| config.scheduler_latency_weight = 0.01,
        |config: &mut Config| config.scheduler_probation_weight = 50.0,
        |config: &mut Config| config.scheduler_selection_pressure_weight = 25.0,
        |config: &mut Config| config.scheduler_total_selection_weight = 1.0,
    ];

    for enable_weight in weight_setters {
        let mut weighted = config.clone();
        enable_weight(&mut weighted);
        assert!(
            scheduler_score_with_config(&worse, None, now_ms, 1.0, &weighted)
                > scheduler_score_with_config(&better, None, now_ms, 0.0, &weighted),
            "启用单个健康调度权重后，较差候选得分应更高"
        );
    }
}

#[test]
fn test_success_updates_health_latency_without_clearing_cooldown() {
    let mut config = Config::default();
    config.credential_stream_error_cooldown_secs = 10;
    let manager =
        MultiTokenManager::new(config, vec![KiroCredentials::default()], None, None, false)
            .unwrap();
    manager
        .report_transient_failure_kind(
            1,
            None,
            TransientFailureKind::Stream,
            None,
            "stream idle timeout",
        )
        .unwrap();
    manager.report_success_with_latency(1, None, Some(StdDuration::from_millis(120)));

    let entry = &manager.snapshot().entries[0];
    assert!(entry.cooled_down);
    assert_eq!(entry.transient_failure_streak, 0);
    assert_eq!(entry.latency_ewma_ms, Some(120.0));
}

#[tokio::test]
async fn test_health_balanced_mode_prefers_best_scored_candidate() {
    let mut config = Config::default();
    config.load_balancing_mode = "health_balanced".to_string();
    config.scheduler_top_k = 1;
    let mut first = KiroCredentials::default();
    first.access_token = Some("first".to_string());
    first.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    let mut second = KiroCredentials::default();
    second.access_token = Some("second".to_string());
    second.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    let manager = MultiTokenManager::new(config, vec![first, second], None, None, false).unwrap();
    {
        let mut entries = manager.entries.lock();
        entries[0].health.recent_error_rate = 1.0;
        entries[0].health.latency_ewma_ms = Some(10_000.0);
    }

    let mut ctx = manager.acquire_context(None).await.unwrap();
    assert_eq!(ctx.id, 2);
    ctx.release_in_flight();
}

#[tokio::test]
async fn test_weighted_least_inflight_mode_prefers_lower_load_candidate() {
    let mut config = Config::default();
    config.load_balancing_mode = "weighted_least_inflight".to_string();
    config.credential_max_concurrent_requests = 10;
    config.scheduler_top_k = 1;
    config.scheduler_priority_weight = 10.0;
    config.scheduler_load_weight = 100.0;
    config.scheduler_error_weight = 0.0;
    config.scheduler_latency_weight = 0.0;
    config.scheduler_probation_weight = 0.0;
    config.scheduler_selection_pressure_weight = 0.0;
    config.scheduler_total_selection_weight = 0.0;
    let manager = MultiTokenManager::new(
        config,
        vec![
            test_access_token_credential("busy-token", "Pro"),
            test_access_token_credential("idle-token", "Pro"),
        ],
        None,
        None,
        false,
    )
    .unwrap();
    {
        let mut entries = manager.entries.lock();
        entries[0].in_flight_requests = 8;
        entries[1].in_flight_requests = 0;
    }

    let mut ctx = manager.acquire_context(None).await.unwrap();

    assert_eq!(ctx.id, 2);
    assert_eq!(ctx.token, "idle-token");
    ctx.release_in_flight();
}

#[tokio::test]
async fn test_health_balanced_mode_penalizes_recent_selection_pressure() {
    let mut config = Config::default();
    config.load_balancing_mode = "health_balanced".to_string();
    config.scheduler_top_k = 1;
    config.scheduler_selection_pressure_weight = 100.0;
    let mut first = KiroCredentials::default();
    first.access_token = Some("first".to_string());
    first.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    let mut second = KiroCredentials::default();
    second.access_token = Some("second".to_string());
    second.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    let manager = MultiTokenManager::new(config, vec![first, second], None, None, false).unwrap();
    {
        let mut entries = manager.entries.lock();
        entries[0].health.recent_selection_count_60s = 100;
    }

    let mut ctx = manager.acquire_context(None).await.unwrap();
    assert_eq!(ctx.id, 2);
    ctx.release_in_flight();
}

#[tokio::test]
async fn test_balanced_mode_rotates_all_warming_credentials_by_recent_selection() {
    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();
    let mut first = KiroCredentials::default();
    first.access_token = Some("first".to_string());
    first.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    let mut second = KiroCredentials::default();
    second.access_token = Some("second".to_string());
    second.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    let mut third = KiroCredentials::default();
    third.access_token = Some("third".to_string());
    third.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    let manager =
        MultiTokenManager::new(config, vec![first, second, third], None, None, false).unwrap();
    manager.set_warmup_remaining(1, 3).unwrap();
    manager.set_warmup_remaining(2, 3).unwrap();
    manager.set_warmup_remaining(3, 3).unwrap();

    let mut seen = Vec::new();
    for _ in 0..3 {
        let mut ctx = manager.acquire_context(None).await.unwrap();
        seen.push(ctx.id);
        ctx.release_in_flight();
    }

    assert_eq!(seen, vec![1, 2, 3]);
    let snapshot = manager.snapshot();
    assert_eq!(
        snapshot
            .entries
            .iter()
            .map(|entry| entry.recent_scheduler_selection_count_60s)
            .collect::<Vec<_>>(),
        vec![1, 1, 1]
    );
}

#[tokio::test]
async fn test_balanced_mode_gives_warming_group_scaled_target_share() {
    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();
    config.credential_warmup_selection_percent = 5;
    config.credential_warmup_max_selection_percent = 50;
    let mut ready = KiroCredentials::default();
    ready.access_token = Some("ready".to_string());
    ready.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    let mut warming_a = KiroCredentials::default();
    warming_a.access_token = Some("warming-a".to_string());
    warming_a.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    let mut warming_b = KiroCredentials::default();
    warming_b.access_token = Some("warming-b".to_string());
    warming_b.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    let manager =
        MultiTokenManager::new(config, vec![ready, warming_a, warming_b], None, None, false)
            .unwrap();
    manager.set_warmup_remaining(2, 3).unwrap();
    manager.set_warmup_remaining(3, 3).unwrap();

    let mut ctx = manager.acquire_context(None).await.unwrap();
    assert_ne!(ctx.id, 1);
    ctx.release_in_flight();
}

#[tokio::test]
async fn test_simulation_balanced_mode_spreads_new_warming_batch() {
    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();
    let credentials = (1_u64..=10)
        .map(|id| api_key_credential(&format!("ksk_warmup_{id}")))
        .collect::<Vec<_>>();
    let manager = MultiTokenManager::new(config, credentials, None, None, false).unwrap();
    for id in 1_u64..=10 {
        manager.set_warmup_remaining(id, 3).unwrap();
    }

    let mut first_round = Vec::new();
    for _ in 0..10 {
        let mut ctx = manager.acquire_context(None).await.unwrap();
        first_round.push(ctx.id);
        ctx.release_in_flight();
    }

    assert_eq!(first_round, (1_u64..=10).collect::<Vec<_>>());

    for _ in 0..40 {
        let mut ctx = manager.acquire_context(None).await.unwrap();
        ctx.release_in_flight();
    }

    let counts = manager
        .snapshot()
        .entries
        .iter()
        .map(|entry| entry.recent_scheduler_selection_count_60s)
        .collect::<Vec<_>>();
    let min = counts.iter().min().copied().unwrap();
    let max = counts.iter().max().copied().unwrap();
    assert!(
        max - min <= 1,
        "新导入预热账号应均衡参与调度，实际近期选中次数: {counts:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_scheduler_handles_500_daily_credentials_1000_rpm_simulation() {
    const CREDENTIAL_COUNT: usize = 500;
    const REQUEST_COUNT: usize = 1000;
    const MAX_ELAPSED: StdDuration = StdDuration::from_secs(5);

    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();
    config.credential_max_concurrent_requests = 0;
    config.dispatch_global_max_concurrent_requests = 0;
    config.dispatch_max_queued_requests = 0;

    let credentials = (1..=CREDENTIAL_COUNT)
        .map(|id| api_key_credential(&format!("ksk_daily_{id}")))
        .collect::<Vec<_>>();
    let manager = Arc::new(MultiTokenManager::new(config, credentials, None, None, false).unwrap());

    let started_at = Instant::now();
    let mut selection_counts: HashMap<u64, usize> = HashMap::new();
    for _ in 0..REQUEST_COUNT {
        let mut ctx =
            tokio::time::timeout(StdDuration::from_secs(1), manager.acquire_context(None))
                .await
                .expect("500 日抛凭据调度不应单次超时")
                .expect("500 日抛凭据调度不应失败");
        *selection_counts.entry(ctx.id).or_insert(0) += 1;
        ctx.release_in_flight();
    }
    let elapsed = started_at.elapsed();

    assert!(
        elapsed <= MAX_ELAPSED,
        "500 日抛凭据、1000 RPM 等价调度耗时过高: {:?} > {:?}",
        elapsed,
        MAX_ELAPSED
    );
    assert_eq!(
        selection_counts.len(),
        CREDENTIAL_COUNT,
        "1000 次调度应覆盖全部 500 个凭据，实际覆盖 {} 个",
        selection_counts.len()
    );
    let min = selection_counts.values().min().copied().unwrap_or_default();
    let max = selection_counts.values().max().copied().unwrap_or_default();
    assert!(
        max - min <= 1,
        "balanced 模式在 500 凭据/1000 次调度下分布应接近均匀，实际 min={min}, max={max}"
    );

    let snapshot = manager.snapshot();
    assert_eq!(snapshot.global_in_flight_requests, 0);
    assert_eq!(snapshot.queued_requests, 0);
}

#[tokio::test]
async fn test_simulation_mixed_large_requests_failures_and_disabled_accounts() {
    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();
    config.credential_max_concurrent_requests = 1;
    config.credential_rate_limit_cooldown_secs = 60;
    config.credential_server_error_cooldown_secs = 60;
    config.credential_max_cooldown_secs = 60;
    config.credential_cooldown_jitter_percent = 0;
    config.credential_dispatch_max_wait_secs = 2;

    let mut credentials = (1_u64..=8)
        .map(|id| api_key_credential(&format!("ksk_mixed_{id}")))
        .collect::<Vec<_>>();
    credentials[1].disabled = true;
    let manager = Arc::new(MultiTokenManager::new(config, credentials, None, None, false).unwrap());

    assert!(manager.report_quota_exhausted(3));
    assert!(manager.report_risk_controlled(
        4,
        CredentialRiskControlReason::TemporarilySuspended,
        "TEMPORARILY_SUSPENDED"
    ));
    assert!(
        manager
            .report_transient_failure_kind(
                5,
                None,
                TransientFailureKind::RateLimit,
                Some(StdDuration::from_secs(30)),
                "429 Too Many Requests",
            )
            .unwrap()
    );
    assert!(
        manager
            .report_transient_failure_kind(
                6,
                None,
                TransientFailureKind::Server,
                Some(StdDuration::from_secs(30)),
                "502 Bad Gateway",
            )
            .unwrap()
    );

    let snapshot = manager.snapshot();
    assert_eq!(snapshot.available, 5);
    for id in [2, 3, 4] {
        let entry = snapshot
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .unwrap();
        assert!(entry.disabled, "凭据 #{id} 应被禁用");
    }
    for id in [5, 6] {
        let entry = snapshot
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .unwrap();
        assert!(entry.cooled_down, "凭据 #{id} 应处于瞬态冷却");
    }

    let mut long_a = manager.acquire_context(None).await.unwrap();
    let mut long_b = manager.acquire_context(None).await.unwrap();
    let mut long_c = manager.acquire_context(None).await.unwrap();
    assert_eq!(vec![long_a.id, long_b.id, long_c.id], vec![1, 7, 8]);

    let waiting_manager = manager.clone();
    let waiting = tokio::spawn(async move { waiting_manager.acquire_context(None).await });
    tokio::time::sleep(StdDuration::from_millis(50)).await;
    assert!(
        !waiting.is_finished(),
        "健康账号都被大请求占满时，后续请求应排队等待"
    );

    long_b.release_in_flight();
    let mut recovered = tokio::time::timeout(StdDuration::from_secs(1), waiting)
        .await
        .expect("释放一个健康账号后等待请求应恢复")
        .expect("等待任务不应 panic")
        .expect("等待请求应成功获取凭据");
    assert_eq!(recovered.id, 7);

    recovered.release_in_flight();
    long_a.release_in_flight();
    long_c.release_in_flight();
    assert_eq!(manager.snapshot().global_in_flight_requests, 0);
}

#[tokio::test]
async fn test_transient_failure_cools_down_only_usable_credential() {
    let mut config = Config::default();
    config.credential_transient_cooldown_secs = 1;
    config.credential_max_cooldown_secs = 1;

    let mut disabled = KiroCredentials::default();
    disabled.disabled = true;
    let mut active = KiroCredentials::default();
    active.access_token = Some("active-token".to_string());
    active.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager = Arc::new(
        MultiTokenManager::new(config, vec![disabled, active], None, None, false).unwrap(),
    );

    assert!(
        !manager
            .report_transient_failure(2, None, Some(StdDuration::from_millis(20)), "429")
            .unwrap()
    );

    let snapshot = manager.snapshot();
    let active = snapshot.entries.iter().find(|entry| entry.id == 2).unwrap();
    assert!(!active.disabled);
    assert_eq!(active.failure_count, 0);
    assert!(active.cooled_down);

    let started = Instant::now();
    let err = match manager.acquire_context(None).await {
        Ok(mut ctx) => {
            ctx.release_in_flight();
            panic!("唯一可用凭据处于上游冷却时应快速失败")
        }
        Err(err) => err.to_string(),
    };
    assert!(
        started.elapsed() < StdDuration::from_millis(200),
        "全部候选都处于上游冷却时不应排队等待"
    );
    assert!(
        err.contains("所有可用账号均处于上游临时冷却"),
        "错误应明确提示全部处于上游临时冷却，实际: {}",
        err
    );
    assert!(
        err.contains("retry_after_secs="),
        "错误应携带 retry_after_secs 供下游快速重试退避，实际: {}",
        err
    );
}

#[tokio::test]
async fn test_rate_limiter_prefers_other_dispatchable_credential() {
    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();
    config.credential_rpm = Some(1);

    let mut cred1 = KiroCredentials::default();
    cred1.access_token = Some("t1".to_string());
    cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    let mut cred2 = KiroCredentials::default();
    cred2.access_token = Some("t2".to_string());
    cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager = MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

    let mut first = manager.acquire_context(None).await.unwrap();
    let mut second = manager.acquire_context(None).await.unwrap();

    assert_ne!(first.id, second.id);
    assert!(
        manager
            .snapshot()
            .entries
            .iter()
            .filter(|entry| entry.rate_limited)
            .count()
            >= 2
    );
    first.release_in_flight();
    second.release_in_flight();
}

#[tokio::test]
async fn test_rate_limiter_blocks_after_window_capacity_is_full() {
    let mut config = Config::default();
    config.credential_rpm = Some(1);

    let mut cred = KiroCredentials::default();
    cred.access_token = Some("t1".to_string());
    cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager = Arc::new(MultiTokenManager::new(config, vec![cred], None, None, false).unwrap());
    let mut first = manager.acquire_context(None).await.unwrap();
    first.mark_upstream_dispatch_started();
    first.release_in_flight();

    let state = manager.local_pool_route_state(None);
    assert_eq!(state.kind, LocalPoolRouteStateKind::AllCoolingDown);
    assert_eq!(state.rate_limit_blocked, 1);
    assert!(state.retry_after_secs.is_some());
}

#[tokio::test]
async fn test_rate_limiter_rolls_back_context_released_before_upstream_dispatch() {
    let mut config = Config::default();
    config.credential_rpm = Some(1);

    let manager = MultiTokenManager::new(
        config,
        vec![test_access_token_credential("t1", "Pro")],
        None,
        None,
        false,
    )
    .unwrap();

    let mut cancelled = manager.acquire_context(None).await.unwrap();
    let credential_id = cancelled.id;
    cancelled.release_in_flight();

    assert!(!manager.snapshot().entries[0].rate_limited);
    let mut next =
        tokio::time::timeout(StdDuration::from_millis(100), manager.acquire_context(None))
            .await
            .expect("pre-dispatch cancellation must not retain the RPM reservation")
            .unwrap();
    assert_eq!(next.id, credential_id);
    next.release_in_flight();
}

#[tokio::test]
async fn test_rate_limiter_paces_idle_requests_instead_of_bursting() {
    let mut config = Config::default();
    config.credential_rpm = Some(6_000);
    config.credential_max_concurrent_requests = 50;
    config.dispatch_global_max_concurrent_requests = 80;

    let manager = MultiTokenManager::new(
        config,
        vec![test_access_token_credential("t1", "Pro")],
        None,
        None,
        false,
    )
    .unwrap();

    let mut first = manager.acquire_context(None).await.unwrap();
    let started = Instant::now();
    let mut second =
        tokio::time::timeout(StdDuration::from_millis(500), manager.acquire_context(None))
            .await
            .expect("pacing interval should expire within the test deadline")
            .unwrap();

    let snapshot = manager.snapshot();
    assert!(started.elapsed() >= StdDuration::from_millis(8));
    assert_eq!(snapshot.global_in_flight_requests, 2);
    assert!(snapshot.entries[0].rate_limited);

    first.release_in_flight();
    second.release_in_flight();
}

#[tokio::test]
async fn test_runtime_config_disabling_credential_rpm_clears_rate_limit_state() {
    let mut config = Config::default();
    config.credential_rpm = Some(1);

    let manager = MultiTokenManager::new(
        config,
        vec![test_access_token_credential("t1", "Pro")],
        None,
        None,
        false,
    )
    .unwrap();

    let mut first = manager.acquire_context(None).await.unwrap();
    first.mark_upstream_dispatch_started();
    first.release_in_flight();
    assert!(manager.snapshot().entries[0].rate_limited);

    manager
        .update_runtime_config(|config| {
            config.credential_rpm = None;
        })
        .unwrap();

    let snapshot = manager.snapshot();
    assert_eq!(snapshot.entries[0].rate_limited, false);
    assert_eq!(manager.runtime_config().credential_rpm, None);

    let mut second = manager.acquire_context(None).await.unwrap();
    assert_eq!(second.id, first.id);
    second.release_in_flight();
}

#[tokio::test]
async fn test_runtime_config_nonzero_rpm_change_drops_old_pacing_deadline() {
    let mut config = Config::default();
    config.credential_rpm = Some(1);
    let manager = MultiTokenManager::new(
        config,
        vec![test_access_token_credential("t1", "Pro")],
        None,
        None,
        false,
    )
    .unwrap();

    let mut first = manager.acquire_context(None).await.unwrap();
    first.mark_upstream_dispatch_started();
    first.release_in_flight();
    assert!(manager.snapshot().entries[0].rate_limited);

    manager
        .update_runtime_config(|config| config.credential_rpm = Some(6_000))
        .unwrap();
    let mut second =
        tokio::time::timeout(StdDuration::from_millis(100), manager.acquire_context(None))
            .await
            .expect("the old one-RPM deadline must not survive a nonzero RPM change")
            .unwrap();
    second.mark_upstream_dispatch_started();
    second.release_in_flight();
}

#[test]
fn test_redis_rate_limit_results_merge_by_deadline_and_reject_old_rpm() {
    let mut config = Config::default();
    config.credential_rpm = Some(6_000);
    let manager = MultiTokenManager::new(
        config,
        vec![test_access_token_credential("t1", "Pro")],
        None,
        None,
        false,
    )
    .unwrap();

    manager.apply_redis_rate_limit_available_at(1, 6_000, 20_000, 50, Some(2));
    manager.apply_redis_rate_limit_available_at(1, 6_000, 10_000, 5_000, Some(1));
    manager.apply_redis_rate_limit_available_at(1, 6_000, 20_000, 500, None);
    {
        let entries = manager.entries.lock();
        assert_eq!(entries[0].rate_limit_redis_deadline_ms, Some(20_000));
        assert_eq!(entries[0].rate_limit_owner_lease_id, Some(2));
    }

    manager
        .update_runtime_config(|config| config.credential_rpm = None)
        .unwrap();
    manager.apply_redis_rate_limit_available_at(1, 6_000, 30_000, 60_000, Some(3));
    let entries = manager.entries.lock();
    assert!(entries[0].rate_limit_available_at.is_none());
    assert!(entries[0].rate_limit_rpm.is_none());
    assert!(entries[0].rate_limit_owner_lease_id.is_none());
    assert!(entries[0].rate_limit_redis_deadline_ms.is_none());
}

#[tokio::test]
async fn test_credential_rpm_override_limits_when_global_unlimited() {
    let mut config = Config::default();
    config.credential_rpm = None;

    let mut cred = test_access_token_credential("t1", "Pro");
    cred.rpm = Some(1);

    let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();
    let mut first = manager.acquire_context(None).await.unwrap();
    first.mark_upstream_dispatch_started();
    first.release_in_flight();

    let snapshot = manager.snapshot();
    assert_eq!(snapshot.entries[0].rpm, 1);
    assert_eq!(snapshot.entries[0].rpm_override, Some(1));
    assert!(snapshot.entries[0].rate_limited);
}

#[tokio::test]
async fn test_credential_rpm_override_zero_bypasses_global_limit() {
    let mut config = Config::default();
    config.credential_rpm = Some(60);

    let mut cred = test_access_token_credential("t1", "Pro");
    cred.rpm = Some(0);

    let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();
    let mut first = manager.acquire_context(None).await.unwrap();
    first.release_in_flight();

    let snapshot = manager.snapshot();
    assert_eq!(snapshot.entries[0].rpm, 0);
    assert_eq!(snapshot.entries[0].rpm_override, Some(0));
    assert!(!snapshot.entries[0].rate_limited);
}

#[tokio::test]
async fn test_all_transient_cooldown_fails_fast() {
    let mut config = Config::default();
    config.credential_transient_cooldown_secs = 1;
    config.credential_max_cooldown_secs = 1;

    let mut cred1 = KiroCredentials::default();
    cred1.access_token = Some("t1".to_string());
    cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    let mut cred2 = KiroCredentials::default();
    cred2.access_token = Some("t2".to_string());
    cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager =
        Arc::new(MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap());
    assert!(
        manager
            .report_transient_failure(1, None, Some(StdDuration::from_millis(20)), "429")
            .unwrap()
    );
    assert!(
        !manager
            .report_transient_failure(2, None, Some(StdDuration::from_millis(20)), "429")
            .unwrap()
    );

    let started = Instant::now();
    let err = match manager.acquire_context(None).await {
        Ok(mut ctx) => {
            ctx.release_in_flight();
            panic!("所有可用凭据都处于上游冷却时应快速失败")
        }
        Err(err) => err.to_string(),
    };

    assert!(
        started.elapsed() < StdDuration::from_millis(200),
        "全账号上游冷却时不应让请求排队等冷却恢复"
    );
    assert!(
        err.contains("所有可用账号均处于上游临时冷却"),
        "错误应明确提示全部处于上游临时冷却，实际: {}",
        err
    );
    assert!(
        err.contains("retry_after_secs="),
        "错误应携带 retry_after_secs，实际: {}",
        err
    );
}

#[tokio::test]
async fn test_concurrency_limiter_prefers_other_dispatchable_credential() {
    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();
    config.credential_max_concurrent_requests = 1;

    let mut cred1 = KiroCredentials::default();
    cred1.access_token = Some("t1".to_string());
    cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    let mut cred2 = KiroCredentials::default();
    cred2.access_token = Some("t2".to_string());
    cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager = MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

    let mut first = manager.acquire_context(None).await.unwrap();
    let mut second = manager.acquire_context(None).await.unwrap();

    assert_ne!(first.id, second.id);
    let snapshot = manager.snapshot();
    assert_eq!(
        snapshot
            .entries
            .iter()
            .map(|entry| entry.in_flight_requests)
            .sum::<u32>(),
        2
    );

    first.release_in_flight();
    let snapshot = manager.snapshot();
    let released = snapshot
        .entries
        .iter()
        .find(|entry| entry.id == first.id)
        .unwrap();
    assert_eq!(released.in_flight_requests, 0);
    second.release_in_flight();
}

#[tokio::test]
async fn test_priority_mode_prefers_lower_in_flight_with_same_priority() {
    let config = Config::default();

    let mut cred1 = KiroCredentials::default();
    cred1.access_token = Some("t1".to_string());
    cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    let mut cred2 = KiroCredentials::default();
    cred2.access_token = Some("t2".to_string());
    cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager = MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

    let mut first = manager.acquire_context(None).await.unwrap();
    let mut second = manager.acquire_context(None).await.unwrap();

    assert_ne!(first.id, second.id);

    first.release_in_flight();
    second.release_in_flight();
}

#[tokio::test]
async fn test_global_capacity_limits_dispatch_and_bounds_wait_queue() {
    let mut config = Config::default();
    config.dispatch_global_max_concurrent_requests = 1;
    config.dispatch_max_queued_requests = 1;
    let mut first_cred = KiroCredentials::default();
    first_cred.access_token = Some("first".to_string());
    first_cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    let mut second_cred = KiroCredentials::default();
    second_cred.access_token = Some("second".to_string());
    second_cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    let manager = Arc::new(
        MultiTokenManager::new(config, vec![first_cred, second_cred], None, None, false).unwrap(),
    );

    let mut first = manager.acquire_context(None).await.unwrap();
    assert_eq!(manager.snapshot().global_in_flight_requests, 1);

    let waiting_manager = manager.clone();
    let waiting = tokio::spawn(async move { waiting_manager.acquire_context(None).await });
    tokio::time::sleep(StdDuration::from_millis(30)).await;
    assert_eq!(manager.snapshot().queued_requests, 1);

    let rejected = match manager.acquire_context(None).await {
        Ok(mut ctx) => {
            ctx.release_in_flight();
            panic!("超过等待队列上限的请求不应获得调度上下文")
        }
        Err(err) => err,
    };
    assert!(rejected.to_string().contains("等待队列已满"));

    first.release_in_flight();
    let mut next = tokio::time::timeout(StdDuration::from_secs(1), waiting)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(manager.snapshot().queued_requests, 0);
    next.release_in_flight();
}

#[tokio::test]
async fn test_fail_fast_global_capacity_full_returns_without_queueing() {
    let mut config = Config::default();
    config.dispatch_global_max_concurrent_requests = 1;
    config.dispatch_max_queued_requests = 10;

    let first_cred = test_access_token_credential("first", "Pro");
    let second_cred = test_access_token_credential("second", "Pro");
    let manager =
        MultiTokenManager::new(config, vec![first_cred, second_cred], None, None, false).unwrap();

    let mut first = manager.acquire_context(None).await.unwrap();
    let err = manager
        .acquire_context_for_session_with_mode(
            None,
            None,
            &HashSet::new(),
            AcquireMode::FailFastOnCapacity,
            1,
        )
        .await
        .err()
        .unwrap()
        .to_string();

    assert!(
        err.contains("本地账号调度容量暂不可用"),
        "fail-fast 模式全局容量满应直接返回容量错误，实际: {}",
        err
    );
    let snapshot = manager.snapshot();
    assert_eq!(snapshot.global_in_flight_requests, 1);
    assert_eq!(snapshot.queued_requests, 0);
    first.release_in_flight();
}

#[tokio::test]
async fn selective_rate_limit_fail_fast_does_not_queue() {
    let mut credential = api_key_credential("selective-rate-limit");
    credential.rpm = Some(60);
    let manager =
        MultiTokenManager::new(Config::default(), vec![credential], None, None, false).unwrap();
    let mut first = manager.acquire_context(None).await.unwrap();
    first.mark_upstream_dispatch_started();
    first.release_in_flight();

    let started_at = Instant::now();
    let err = manager
        .acquire_context_for_session_with_mode(
            None,
            None,
            &HashSet::new(),
            AcquireMode::SelectiveFailFast {
                rate_limit: true,
                concurrency: false,
            },
            1,
        )
        .await
        .err()
        .expect("rate-limited selective fail-fast request must return an error");

    assert!(started_at.elapsed() < StdDuration::from_millis(250));
    assert_eq!(manager.snapshot().queued_requests, 0);
    let summary = manager.selection_failure_summary(
        "selective-rate-limit",
        "local_account",
        None,
        &err.to_string(),
    );
    assert_eq!(summary.primary_reason, AccountRejectReason::RpmLimited);
}

#[tokio::test]
async fn selective_rate_limit_fail_fast_still_waits_for_concurrency() {
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 1;
    let manager = Arc::new(
        MultiTokenManager::new(
            config,
            vec![api_key_credential("selective-concurrency-wait")],
            None,
            None,
            false,
        )
        .unwrap(),
    );
    let mut first = manager.acquire_context(None).await.unwrap();
    let waiting_manager = manager.clone();
    let waiting = tokio::spawn(async move {
        waiting_manager
            .acquire_context_for_session_with_mode(
                None,
                None,
                &HashSet::new(),
                AcquireMode::SelectiveFailFast {
                    rate_limit: true,
                    concurrency: false,
                },
                1,
            )
            .await
    });

    tokio::time::sleep(StdDuration::from_millis(40)).await;
    assert!(!waiting.is_finished());
    assert_eq!(manager.snapshot().queued_requests, 1);
    first.release_in_flight();

    let mut second = tokio::time::timeout(StdDuration::from_secs(1), waiting)
        .await
        .expect("rate-only fail-fast request must resume after concurrency is released")
        .unwrap()
        .unwrap();
    assert_eq!(manager.snapshot().queued_requests, 0);
    second.release_in_flight();
}

#[tokio::test]
async fn selective_mixed_pressure_waits_for_concurrency_before_rate_fallback() {
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 1;
    let mut credential = api_key_credential("selective-mixed-pressure");
    credential.rpm = Some(60);
    let manager =
        Arc::new(MultiTokenManager::new(config, vec![credential], None, None, false).unwrap());
    let mut first = manager.acquire_context(None).await.unwrap();
    first.mark_upstream_dispatch_started();
    let mixed_state = manager.compute_local_pool_route_state(None);
    assert_eq!(mixed_state.kind, LocalPoolRouteStateKind::CapacityFull);
    assert_eq!(mixed_state.rate_limit_blocked, 1);
    assert_eq!(mixed_state.concurrency_blocked, 1);

    let waiting_manager = manager.clone();
    let waiting = tokio::spawn(async move {
        waiting_manager
            .acquire_context_for_session_with_mode(
                None,
                None,
                &HashSet::new(),
                AcquireMode::SelectiveFailFast {
                    rate_limit: true,
                    concurrency: false,
                },
                1,
            )
            .await
    });
    tokio::time::sleep(StdDuration::from_millis(40)).await;
    assert!(!waiting.is_finished());
    assert_eq!(manager.snapshot().queued_requests, 1);

    first.release_in_flight();
    let result = tokio::time::timeout(StdDuration::from_secs(1), waiting)
        .await
        .expect("request must be reclassified promptly after concurrency clears")
        .unwrap();
    let error = match result {
        Ok(mut context) => {
            context.release_in_flight();
            panic!("remaining RPM pressure should become the external fallback signal");
        }
        Err(error) => error,
    };
    assert!(error.to_string().contains("RPM 调度暂不可用"));
    assert_eq!(manager.snapshot().queued_requests, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redis_capacity_admission_is_not_reclassified_as_rate_limit() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 1;
    let mut credential = api_key_credential("redis-mixed-pressure");
    credential.id = Some(1);
    credential.rpm = Some(60);
    let manager_a = MultiTokenManager::new_with_stores(
        config.clone(),
        vec![credential.clone()],
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();
    let manager_b = Arc::new(
        MultiTokenManager::new_with_stores(
            config,
            vec![credential],
            None,
            None,
            false,
            None,
            Some(redis_store.clone()),
        )
        .unwrap(),
    );

    let mut first = manager_a.acquire_context(None).await.unwrap();
    first.mark_upstream_dispatch_started();
    *manager_b.last_scheduler_redis_sync_at.lock() = Some(Instant::now());
    let waiting_manager = manager_b.clone();
    let waiting = tokio::spawn(async move {
        waiting_manager
            .acquire_context_for_session_with_mode(
                None,
                None,
                &HashSet::new(),
                AcquireMode::SelectiveFailFast {
                    rate_limit: true,
                    concurrency: false,
                },
                1,
            )
            .await
    });

    tokio::time::sleep(StdDuration::from_millis(50)).await;
    assert!(
        !waiting.is_finished(),
        "Redis Lua CapacityFull 必须按并发压力等待，不能被本地 RPM 镜像误判后 fail-fast"
    );
    first.release_in_flight();
    let result = tokio::time::timeout(StdDuration::from_secs(2), waiting)
        .await
        .expect("request should be reclassified after distributed concurrency clears")
        .unwrap();
    match result {
        Ok(mut context) => {
            context.release_in_flight();
        }
        Err(error) => assert!(error.to_string().contains("RPM 调度暂不可用")),
    }
    redis_store.clear_rate_limit(1).await.unwrap();
    redis_store.clear_in_flight_leases(1, None).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slot_race_exclusion_is_restored_before_waiting_on_another_credential() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 1;
    let mut paced = api_key_credential("paced");
    paced.id = Some(1);
    paced.rpm = Some(60);
    paced.priority = 1;
    let mut busy = api_key_credential("busy");
    busy.id = Some(2);
    busy.rpm = Some(0);
    busy.priority = 2;
    let mut caller_excluded = api_key_credential("caller-excluded");
    caller_excluded.id = Some(3);
    caller_excluded.rpm = Some(0);
    caller_excluded.priority = 0;

    let pacing_manager = MultiTokenManager::new_with_stores(
        config.clone(),
        vec![paced.clone()],
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();
    let manager = Arc::new(
        MultiTokenManager::new_with_stores(
            config,
            vec![paced, busy, caller_excluded],
            None,
            None,
            false,
            None,
            Some(redis_store.clone()),
        )
        .unwrap(),
    );

    let mut paced_context = pacing_manager.acquire_context(None).await.unwrap();
    paced_context.mark_upstream_dispatch_started();
    paced_context.release_in_flight();
    let busy_lease = manager
        .acquire_in_flight_lease_for_test(2)
        .expect("second credential should be held at local concurrency capacity");
    *manager.last_scheduler_redis_sync_at.lock() = Some(Instant::now());

    let waiting_manager = manager.clone();
    let waiting = tokio::spawn(async move {
        let caller_excluded_ids = HashSet::from([3]);
        waiting_manager
            .acquire_context_for_session_with_mode(
                None,
                None,
                &caller_excluded_ids,
                AcquireMode::SelectiveFailFast {
                    rate_limit: true,
                    concurrency: false,
                },
                1,
            )
            .await
    });
    let mut context = tokio::time::timeout(StdDuration::from_secs(2), waiting)
        .await
        .expect("paced credential must be reconsidered after its Redis deadline expires")
        .unwrap()
        .unwrap();
    assert_eq!(context.id, 1);
    context.release_in_flight();
    drop(busy_lease);
    redis_store.clear_rate_limit(1).await.unwrap();
    redis_store.clear_in_flight_leases(1, None).await.unwrap();
}

#[tokio::test]
async fn test_weighted_local_capacity_consumes_single_credential_slots() {
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 4;

    let manager = MultiTokenManager::new(
        config,
        vec![test_access_token_credential("weighted", "Pro")],
        None,
        None,
        false,
    )
    .unwrap();

    let mut first = manager
        .acquire_context_for_session_with_mode(
            None,
            None,
            &HashSet::new(),
            AcquireMode::FailFastOnCapacity,
            4,
        )
        .await
        .unwrap();

    let snapshot = manager.snapshot();
    assert_eq!(snapshot.entries[0].in_flight_requests, 4);
    assert_eq!(snapshot.global_in_flight_requests, 4);

    let err = manager
        .acquire_context_for_session_with_mode(
            None,
            None,
            &HashSet::new(),
            AcquireMode::FailFastOnCapacity,
            1,
        )
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(
        err.contains("本地账号调度容量暂不可用"),
        "单账号 weighted 容量满应直接返回容量错误，实际: {err}"
    );

    first.release_in_flight();
    let snapshot = manager.snapshot();
    assert_eq!(snapshot.entries[0].in_flight_requests, 0);
    assert_eq!(snapshot.global_in_flight_requests, 0);
}

#[tokio::test]
async fn test_weighted_local_capacity_consumes_global_slots() {
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 8;
    config.dispatch_global_max_concurrent_requests = 4;

    let manager = MultiTokenManager::new(
        config,
        vec![
            test_access_token_credential("weighted-a", "Pro"),
            test_access_token_credential("weighted-b", "Pro"),
        ],
        None,
        None,
        false,
    )
    .unwrap();

    let mut first = manager
        .acquire_context_for_session_with_mode(
            None,
            None,
            &HashSet::new(),
            AcquireMode::FailFastOnCapacity,
            4,
        )
        .await
        .unwrap();
    assert_eq!(manager.snapshot().global_in_flight_requests, 4);

    let err = manager
        .acquire_context_for_session_with_mode(
            None,
            None,
            &HashSet::new(),
            AcquireMode::FailFastOnCapacity,
            1,
        )
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(
        err.contains("本地账号调度容量暂不可用"),
        "全局 weighted 容量满应直接返回容量错误，实际: {err}"
    );

    first.release_in_flight();
    assert_eq!(manager.snapshot().global_in_flight_requests, 0);
}

#[tokio::test]
async fn test_weighted_selection_pressure_counts_capacity_units_not_total_requests() {
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 8;
    config.scheduler_selection_pressure_weight = 25.0;

    let manager = MultiTokenManager::new(
        config,
        vec![test_access_token_credential("weighted-selection", "Pro")],
        None,
        None,
        false,
    )
    .unwrap();

    let mut ctx = manager
        .acquire_context_for_session_with_mode(
            None,
            None,
            &HashSet::new(),
            AcquireMode::FailFastOnCapacity,
            4,
        )
        .await
        .unwrap();
    ctx.mark_upstream_dispatch_started();
    ctx.release_in_flight();

    let snapshot = manager.snapshot();
    assert_eq!(snapshot.entries[0].scheduler_selection_count, 1);
    assert_eq!(snapshot.entries[0].recent_scheduler_selection_count_10s, 4);
    assert_eq!(snapshot.entries[0].recent_scheduler_selection_count_60s, 4);
    assert_eq!(snapshot.entries[0].recent_scheduler_selection_count_5m, 4);
}

#[tokio::test]
async fn test_weighted_rpm_consumes_capacity_units() {
    let mut config = Config::default();
    config.credential_rpm = Some(4);
    config.credential_max_concurrent_requests = 8;

    let manager = MultiTokenManager::new(
        config,
        vec![test_access_token_credential("weighted-rpm", "Pro")],
        None,
        None,
        false,
    )
    .unwrap();

    let mut ctx = manager
        .acquire_context_for_session_with_mode(
            None,
            None,
            &HashSet::new(),
            AcquireMode::FailFastOnCapacity,
            4,
        )
        .await
        .unwrap();
    ctx.mark_upstream_dispatch_started();
    ctx.release_in_flight();

    let state = manager.local_pool_route_state(None);
    assert_eq!(state.kind, LocalPoolRouteStateKind::AllCoolingDown);
    assert_eq!(state.rate_limit_blocked, 1);
    assert!(manager.snapshot().entries[0].rate_limited);
}

#[test]
fn test_local_pool_route_state_reports_no_credentials_and_all_disabled() {
    let empty = MultiTokenManager::new(Config::default(), vec![], None, None, false).unwrap();
    let empty_state = empty.local_pool_route_state(None);
    assert_eq!(empty_state.kind, LocalPoolRouteStateKind::NoCredentials);
    assert_eq!(empty_state.total, 0);
    assert_eq!(empty_state.available, 0);

    let mut disabled = test_access_token_credential("disabled", "Pro");
    disabled.disabled = true;
    let disabled_manager =
        MultiTokenManager::new(Config::default(), vec![disabled], None, None, false).unwrap();
    let disabled_state = disabled_manager.local_pool_route_state(None);
    assert_eq!(disabled_state.kind, LocalPoolRouteStateKind::AllDisabled);
    assert_eq!(disabled_state.total, 1);
    assert_eq!(disabled_state.available, 0);
}

#[tokio::test]
async fn test_local_pool_route_state_reports_capacity_full_without_queueing() {
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 1;
    config.dispatch_max_queued_requests = 10;
    let manager = MultiTokenManager::new(
        config,
        vec![test_access_token_credential("first", "Pro")],
        None,
        None,
        false,
    )
    .unwrap();

    let ready = manager.local_pool_route_state(None);
    assert_eq!(ready.kind, LocalPoolRouteStateKind::Ready);
    assert_eq!(ready.dispatchable, 1);

    let mut ctx = manager.acquire_context(None).await.unwrap();
    let full = manager.local_pool_route_state(None);
    assert_eq!(full.kind, LocalPoolRouteStateKind::CapacityFull);
    assert_eq!(full.dispatchable, 0);
    assert_eq!(full.concurrency_blocked, 1);
    assert_eq!(full.queued_requests, 0);

    ctx.release_in_flight();
    let ready_again = manager.local_pool_route_state(None);
    assert_eq!(ready_again.kind, LocalPoolRouteStateKind::Ready);
    assert_eq!(ready_again.dispatchable, 1);
}

#[tokio::test]
async fn selection_failure_summary_records_concurrency_full_accounts() {
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 1;
    let manager = MultiTokenManager::new(
        config,
        vec![test_access_token_credential("first", "Pro")],
        None,
        None,
        false,
    )
    .unwrap();

    let mut ctx = manager.acquire_context(None).await.unwrap();
    let summary = manager.selection_failure_summary(
        "req_concurrency",
        "/cc/v1/messages",
        None,
        "账号调度排队等待超时",
    );

    assert_eq!(summary.stage, SelectionFailureStage::DispatchWait);
    assert_eq!(
        summary.primary_reason,
        AccountRejectReason::AccountConcurrencyFull
    );
    assert_eq!(
        summary
            .reason_counts
            .get(&AccountRejectReason::AccountConcurrencyFull),
        Some(&1)
    );
    assert_eq!(summary.sampled_accounts.len(), 1);
    assert_eq!(
        summary.sampled_accounts[0].reason,
        AccountRejectReason::AccountConcurrencyFull
    );

    ctx.release_in_flight();
}

#[tokio::test]
async fn selection_failure_summary_records_rpm_limited_accounts() {
    let mut config = Config::default();
    config.credential_rpm = Some(1);
    let manager = MultiTokenManager::new(
        config,
        vec![test_access_token_credential("first", "Pro")],
        None,
        None,
        false,
    )
    .unwrap();

    let mut ctx = manager.acquire_context(None).await.unwrap();
    ctx.mark_upstream_dispatch_started();
    ctx.release_in_flight();
    let summary = manager.selection_failure_summary(
        "req_rpm",
        "/cc/v1/messages",
        None,
        "本地限流 retry_after_secs=1",
    );

    assert_eq!(summary.stage, SelectionFailureStage::RpmLimit);
    assert_eq!(summary.primary_reason, AccountRejectReason::RpmLimited);
    assert_eq!(
        summary.reason_counts.get(&AccountRejectReason::RpmLimited),
        Some(&1)
    );
    assert_eq!(summary.waitable_account_count, 1);
    assert!(summary.retry_after_ms.is_some());
}

#[tokio::test]
async fn selection_failure_summary_records_model_not_supported() {
    let mut free = api_key_credential("ksk_selection_free");
    free.subscription_title = Some("Free".to_string());
    let manager = MultiTokenManager::new(Config::default(), vec![free], None, None, false).unwrap();

    let summary = manager.selection_failure_summary(
        "req_model",
        "/cc/v1/messages",
        Some("claude-opus-4-8"),
        "没有支持当前模型的可用账号",
    );

    assert_eq!(summary.stage, SelectionFailureStage::ModelEligibility);
    assert_eq!(
        summary.primary_reason,
        AccountRejectReason::ModelNotSupported
    );
    assert_eq!(
        summary
            .reason_counts
            .get(&AccountRejectReason::ModelNotSupported),
        Some(&1)
    );
}

#[test]
fn selection_failure_summary_enforces_sample_limit_and_omits_secrets() {
    let credentials = (0..100)
        .map(|idx| {
            let mut credential = api_key_credential(&format!("ksk_secret_selection_{idx:03}"));
            credential.disabled = true;
            credential
        })
        .collect::<Vec<_>>();
    let manager =
        MultiTokenManager::new(Config::default(), credentials, None, None, false).unwrap();

    let summary = manager.selection_failure_summary(
        "req_disabled",
        "/cc/v1/messages",
        None,
        "所有账号均已禁用",
    );

    assert_eq!(summary.rejected_account_count, 100);
    assert_eq!(
        summary.sampled_accounts.len(),
        Config::default().selection_failure_sample_limit
    );
    let serialized = serde_json::to_string(&summary).unwrap();
    assert!(!serialized.contains("ksk_secret_selection"));
    assert!(!serialized.contains("access_token"));
    assert!(!serialized.contains("refresh_token"));
}

#[test]
fn selection_failure_summary_can_disable_account_samples() {
    let mut config = Config::default();
    config.selection_failure_record_enabled = false;
    config.selection_failure_sample_limit = 20;
    let credentials = (0..10)
        .map(|idx| {
            let mut credential = api_key_credential(&format!("ksk_secret_selection_{idx:03}"));
            credential.disabled = true;
            credential
        })
        .collect::<Vec<_>>();
    let manager = MultiTokenManager::new(config, credentials, None, None, false).unwrap();

    let summary = manager.selection_failure_summary(
        "req_disabled_no_samples",
        "/cc/v1/messages",
        None,
        "所有账号均已禁用",
    );

    assert_eq!(summary.rejected_account_count, 10);
    assert!(summary.sampled_accounts.is_empty());
    assert_eq!(
        summary.reason_counts.get(&AccountRejectReason::Disabled),
        Some(&10)
    );
}

#[tokio::test]
async fn test_local_pool_route_state_sees_added_credential_after_empty_pool() {
    let manager = MultiTokenManager::new(Config::default(), vec![], None, None, false).unwrap();
    let empty_state = manager.local_pool_route_state(None);
    assert_eq!(empty_state.kind, LocalPoolRouteStateKind::NoCredentials);

    manager
        .add_credential(api_key_credential("ksk_dynamic_added"))
        .await
        .unwrap();

    let ready = manager.local_pool_route_state(None);
    assert_eq!(ready.kind, LocalPoolRouteStateKind::Ready);
    assert_eq!(ready.total, 1);
    assert_eq!(ready.dispatchable, 1);
}

#[test]
fn test_local_pool_route_state_sees_manual_enable_after_all_disabled() {
    let mut disabled = api_key_credential("ksk_dynamic_disabled");
    disabled.disabled = true;
    let manager =
        MultiTokenManager::new(Config::default(), vec![disabled], None, None, false).unwrap();

    let disabled_state = manager.local_pool_route_state(None);
    assert_eq!(disabled_state.kind, LocalPoolRouteStateKind::AllDisabled);

    manager.set_disabled(1, false).unwrap();

    let ready = manager.local_pool_route_state(None);
    assert_eq!(ready.kind, LocalPoolRouteStateKind::Ready);
    assert_eq!(ready.available, 1);
    assert_eq!(ready.dispatchable, 1);
}

#[tokio::test]
async fn test_local_pool_route_state_sees_model_compatible_credential_added() {
    let mut free = api_key_credential("ksk_model_free");
    free.subscription_title = Some("Free".to_string());
    let manager = MultiTokenManager::new(Config::default(), vec![free], None, None, false).unwrap();

    let unsupported = manager.local_pool_route_state(Some("claude-opus-4-8"));
    assert_eq!(unsupported.kind, LocalPoolRouteStateKind::NoModelCompatible);

    let mut pro = api_key_credential("ksk_model_pro");
    pro.subscription_title = Some("Pro".to_string());
    manager.add_credential(pro).await.unwrap();

    let ready = manager.local_pool_route_state(Some("claude-opus-4-8"));
    assert_eq!(ready.kind, LocalPoolRouteStateKind::Ready);
    assert_eq!(ready.model_usable, 1);
    assert_eq!(ready.dispatchable, 1);
}

#[tokio::test]
async fn local_pool_route_state_cache_expires_at_rate_limit_deadline() {
    let mut credential = api_key_credential("route-state-paced");
    credential.rpm = Some(60);
    let manager =
        MultiTokenManager::new(Config::default(), vec![credential], None, None, false).unwrap();
    {
        let mut entries = manager.entries.lock();
        entries[0].rate_limit_available_at = Some(Instant::now() + StdDuration::from_millis(80));
        entries[0].rate_limit_rpm = Some(60);
    }

    let blocked = manager.local_pool_route_state(None);
    assert_eq!(blocked.kind, LocalPoolRouteStateKind::AllCoolingDown);
    assert_eq!(blocked.rate_limit_blocked, 1);

    tokio::time::sleep(StdDuration::from_millis(100)).await;
    let ready = manager.local_pool_route_state(None);
    assert_eq!(
        ready.kind,
        LocalPoolRouteStateKind::Ready,
        "time-driven pacing expiry must not remain hidden behind the 250ms route-state cache"
    );
}

#[test]
fn test_local_pool_route_state_auto_heals_too_many_failures() {
    let manager = MultiTokenManager::new(
        Config::default(),
        vec![api_key_credential("ksk_auto_heal_preflight")],
        None,
        None,
        false,
    )
    .unwrap();

    assert!(manager.report_failure(1));
    assert!(manager.report_failure(1));
    assert!(!manager.report_failure(1));
    let disabled = manager.snapshot().entries.into_iter().next().unwrap();
    assert!(disabled.disabled);
    assert_eq!(
        disabled.disabled_reason.as_deref(),
        Some(DisabledReason::TooManyFailures.as_str())
    );

    let ready = manager.local_pool_route_state(None);
    assert_eq!(ready.kind, LocalPoolRouteStateKind::Ready);
    assert_eq!(ready.available, 1);
    assert_eq!(ready.dispatchable, 1);

    let healed = manager.snapshot().entries.into_iter().next().unwrap();
    assert!(!healed.disabled);
    assert_eq!(healed.failure_count, 0);
    assert!(healed.disabled_reason.is_none());
}

#[tokio::test]
async fn test_local_pool_route_state_proxy_blocked_recovers_after_resource_enabled() {
    let mut credential = api_key_credential("ksk_proxy_dynamic");
    credential.proxy_resource_id = Some(7);
    let manager =
        MultiTokenManager::new(Config::default(), vec![credential], None, None, false).unwrap();
    manager.proxy_resources.lock().insert(
        7,
        ProxyResourceRuntime {
            id: 7,
            name: "residential".to_string(),
            proxy_url: "http://127.0.0.1:8080".to_string(),
            proxy_username: None,
            proxy_password: None,
            enabled: false,
        },
    );
    manager.invalidate_local_pool_route_state_cache();

    let blocked = manager.local_pool_route_state(None);
    assert_eq!(blocked.kind, LocalPoolRouteStateKind::ProxyBlocked);
    assert_eq!(blocked.proxy_blocked, 1);

    manager.proxy_resources.lock().get_mut(&7).unwrap().enabled = true;
    manager.invalidate_local_pool_route_state_cache();

    let ready = manager.local_pool_route_state(None);
    assert_eq!(ready.kind, LocalPoolRouteStateKind::Ready);
    assert_eq!(ready.dispatchable, 1);

    let mut ctx = manager.acquire_context(None).await.unwrap();
    assert_eq!(ctx.id, 1);
    ctx.release_in_flight();
}

#[tokio::test]
async fn test_concurrency_limiter_skips_disabled_credentials_and_queues_on_only_active() {
    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();
    config.credential_max_concurrent_requests = 1;

    let mut disabled1 = KiroCredentials::default();
    disabled1.disabled = true;
    let mut disabled2 = KiroCredentials::default();
    disabled2.disabled = true;
    let mut active = KiroCredentials::default();
    active.access_token = Some("active-token".to_string());
    active.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager = Arc::new(
        MultiTokenManager::new(
            config,
            vec![disabled1, disabled2, active],
            None,
            None,
            false,
        )
        .unwrap(),
    );
    assert_eq!(manager.available_count(), 1);

    let mut first = manager.acquire_context(None).await.unwrap();
    assert_eq!(first.id, 3);

    let waiting_manager = manager.clone();
    let waiting = tokio::spawn(async move { waiting_manager.acquire_context(None).await });
    tokio::time::sleep(StdDuration::from_millis(50)).await;
    assert!(
        !waiting.is_finished(),
        "只剩一个启用凭据且并发占满时，后续请求应排队等待"
    );

    first.release_in_flight();
    let mut second = tokio::time::timeout(StdDuration::from_secs(1), waiting)
        .await
        .expect("释放唯一启用凭据后等待请求应被唤醒")
        .expect("等待任务不应 panic")
        .expect("等待请求应成功获取唯一启用凭据");

    assert_eq!(second.id, 3);
    second.release_in_flight();

    let snapshot = manager.snapshot();
    assert_eq!(
        snapshot
            .entries
            .iter()
            .map(|entry| entry.in_flight_requests)
            .sum::<u32>(),
        0
    );
}

#[tokio::test]
async fn test_concurrency_limiter_multiple_waiters_are_served_serially_on_one_credential() {
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 1;

    let mut cred = KiroCredentials::default();
    cred.access_token = Some("t1".to_string());
    cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager = Arc::new(MultiTokenManager::new(config, vec![cred], None, None, false).unwrap());
    let mut first = manager.acquire_context(None).await.unwrap();

    let (acquired_tx, mut acquired_rx) = tokio::sync::mpsc::channel::<(&'static str, u64)>(2);
    let (release_a_tx, release_a_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_b_tx, release_b_rx) = tokio::sync::oneshot::channel::<()>();
    let mut release_a_tx = Some(release_a_tx);
    let mut release_b_tx = Some(release_b_tx);

    let waiting_a_manager = manager.clone();
    let waiting_a_tx = acquired_tx.clone();
    let waiting_a = tokio::spawn(async move {
        let mut ctx = waiting_a_manager.acquire_context(None).await.unwrap();
        waiting_a_tx.send(("a", ctx.id)).await.unwrap();
        let _ = release_a_rx.await;
        ctx.release_in_flight();
    });
    let waiting_b_manager = manager.clone();
    let waiting_b_tx = acquired_tx;
    let waiting_b = tokio::spawn(async move {
        let mut ctx = waiting_b_manager.acquire_context(None).await.unwrap();
        waiting_b_tx.send(("b", ctx.id)).await.unwrap();
        let _ = release_b_rx.await;
        ctx.release_in_flight();
    });

    tokio::time::sleep(StdDuration::from_millis(50)).await;
    assert!(
        matches!(
            acquired_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ),
        "首个占用未释放前，两个等待者都不应获得并发槽"
    );

    first.release_in_flight();

    let (second_label, second_id) =
        tokio::time::timeout(StdDuration::from_secs(1), acquired_rx.recv())
            .await
            .expect("第一个等待者应在释放后获得并发槽")
            .expect("等待者应发送获取结果");
    assert_eq!(second_id, 1);
    assert_eq!(manager.snapshot().entries[0].in_flight_requests, 1);

    tokio::time::sleep(StdDuration::from_millis(50)).await;
    assert!(
        matches!(
            acquired_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ),
        "第二个等待者应继续排队，不能和第一个等待者同时占用同一凭据"
    );

    if second_label == "a" {
        release_a_tx.take().unwrap().send(()).unwrap();
    } else {
        release_b_tx.take().unwrap().send(()).unwrap();
    }

    let (third_label, third_id) =
        tokio::time::timeout(StdDuration::from_secs(1), acquired_rx.recv())
            .await
            .expect("第二个等待者应在前一个请求释放后获得并发槽")
            .expect("等待者应发送获取结果");
    assert_eq!(third_id, 1);

    if third_label == "a" {
        release_a_tx.take().unwrap().send(()).unwrap();
    } else {
        release_b_tx.take().unwrap().send(()).unwrap();
    }

    tokio::time::timeout(StdDuration::from_secs(1), waiting_a)
        .await
        .expect("等待任务 a 应正常结束")
        .expect("等待任务 a 不应 panic");
    tokio::time::timeout(StdDuration::from_secs(1), waiting_b)
        .await
        .expect("等待任务 b 应正常结束")
        .expect("等待任务 b 不应 panic");

    assert_eq!(manager.snapshot().entries[0].in_flight_requests, 0);
}

#[tokio::test]
async fn test_acquire_context_all_manually_disabled_fails_without_queueing() {
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 1;
    config.credential_dispatch_max_wait_secs = 1;

    let mut disabled1 = KiroCredentials::default();
    disabled1.disabled = true;
    let mut disabled2 = KiroCredentials::default();
    disabled2.disabled = true;

    let manager =
        MultiTokenManager::new(config, vec![disabled1, disabled2], None, None, false).unwrap();
    assert_eq!(manager.available_count(), 0);

    let started = Instant::now();
    let err = manager
        .acquire_context(None)
        .await
        .err()
        .unwrap()
        .to_string();

    assert!(
        started.elapsed() < StdDuration::from_millis(200),
        "全部手动禁用不是临时调度阻塞，不应进入并发排队等待"
    );
    assert!(
        err.contains("所有账号均已禁用"),
        "错误应明确提示全部禁用，实际: {}",
        err
    );
}

#[tokio::test]
async fn test_all_model_incompatible_credentials_fail_fast_without_queueing() {
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 1;
    config.credential_dispatch_max_wait_secs = 1;

    let free_a = test_access_token_credential("free-a", "Free");
    let free_b = test_access_token_credential("free-b", "Free");
    let manager = MultiTokenManager::new(config, vec![free_a, free_b], None, None, false).unwrap();

    let started = Instant::now();
    let err = manager
        .acquire_context(Some("claude-opus-4"))
        .await
        .err()
        .unwrap()
        .to_string();

    assert!(
        started.elapsed() < StdDuration::from_millis(200),
        "模型不兼容不是临时容量阻塞，不应进入等待队列"
    );
    assert!(
        err.contains("没有支持当前模型的可用账号"),
        "错误应明确提示模型不兼容，实际: {}",
        err
    );
    assert_eq!(manager.snapshot().queued_requests, 0);
}

#[tokio::test]
async fn test_concurrency_limiter_waits_until_slot_released() {
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 1;

    let mut cred = KiroCredentials::default();
    cred.access_token = Some("t1".to_string());
    cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager = Arc::new(MultiTokenManager::new(config, vec![cred], None, None, false).unwrap());
    let mut first = manager.acquire_context(None).await.unwrap();

    let waiting_manager = manager.clone();
    let waiting = tokio::spawn(async move { waiting_manager.acquire_context(None).await });
    tokio::time::sleep(StdDuration::from_millis(50)).await;
    assert!(
        !waiting.is_finished(),
        "并发占满时请求应排队等待，而不是立即返回"
    );

    first.release_in_flight();
    let mut second = tokio::time::timeout(StdDuration::from_secs(1), waiting)
        .await
        .expect("释放并发槽后等待请求应被唤醒")
        .expect("等待任务不应 panic")
        .expect("等待请求应成功获取凭据");

    assert_eq!(second.id, first.id);
    second.release_in_flight();
}

#[tokio::test]
async fn test_fail_fast_slot_race_reselects_other_available_credential() {
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 1;

    let mut first_cred = KiroCredentials::default();
    first_cred.access_token = Some("t1".to_string());
    first_cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    first_cred.priority = 0;

    let mut second_cred = KiroCredentials::default();
    second_cred.access_token = Some("t2".to_string());
    second_cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    second_cred.priority = 0;

    let manager =
        MultiTokenManager::new(config, vec![first_cred, second_cred], None, None, false).unwrap();
    let mut first = manager.acquire_context(None).await.unwrap();
    assert_eq!(first.id, 1);

    let mut second = manager
        .acquire_context_for_session_with_mode(
            None,
            None,
            &HashSet::new(),
            AcquireMode::FailFastOnCapacity,
            1,
        )
        .await
        .expect("fail-fast should reselect another credential when the selected slot is full");

    assert_eq!(second.id, 2);
    let snapshot = manager.snapshot();
    assert_eq!(snapshot.entries[0].in_flight_requests, 1);
    assert_eq!(snapshot.entries[1].in_flight_requests, 1);

    first.release_in_flight();
    second.release_in_flight();
}

#[tokio::test]
async fn test_credential_concurrency_override_limits_when_global_unlimited() {
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 0;

    let mut cred = KiroCredentials::default();
    cred.access_token = Some("t1".to_string());
    cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    cred.max_concurrent_requests = Some(1);

    let manager = Arc::new(MultiTokenManager::new(config, vec![cred], None, None, false).unwrap());
    let mut first = manager.acquire_context(None).await.unwrap();
    let snapshot = manager.snapshot();
    assert_eq!(snapshot.entries[0].max_concurrent_requests, 1);
    assert_eq!(
        snapshot.entries[0].max_concurrent_requests_override,
        Some(1)
    );

    let waiting_manager = manager.clone();
    let waiting = tokio::spawn(async move { waiting_manager.acquire_context(None).await });
    tokio::time::sleep(StdDuration::from_millis(50)).await;
    assert!(
        !waiting.is_finished(),
        "账号级并发覆盖为 1 时，即使全局不限，也应排队等待"
    );

    first.release_in_flight();
    let mut second = tokio::time::timeout(StdDuration::from_secs(1), waiting)
        .await
        .expect("释放账号级并发槽后等待请求应恢复")
        .expect("等待任务不应 panic")
        .expect("等待请求应成功获取凭据");
    second.release_in_flight();
}

#[tokio::test]
async fn test_credential_concurrency_override_zero_bypasses_global_limit() {
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 1;

    let mut cred = KiroCredentials::default();
    cred.access_token = Some("t1".to_string());
    cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    cred.max_concurrent_requests = Some(0);

    let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();
    let mut first = manager.acquire_context(None).await.unwrap();
    let mut second = manager.acquire_context(None).await.unwrap();
    let snapshot = manager.snapshot();
    assert_eq!(snapshot.entries[0].in_flight_requests, 2);
    assert_eq!(snapshot.entries[0].max_concurrent_requests, 0);
    assert_eq!(
        snapshot.entries[0].max_concurrent_requests_override,
        Some(0)
    );
    first.release_in_flight();
    second.release_in_flight();
}

#[tokio::test]
async fn test_credential_concurrency_override_exceeds_global_default() {
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 5;

    let mut cred = KiroCredentials::default();
    cred.access_token = Some("t1".to_string());
    cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    cred.max_concurrent_requests = Some(200);

    let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();
    let mut leases = Vec::new();
    for _ in 0..6 {
        leases.push(manager.acquire_context(None).await.unwrap());
    }

    let runtime_snapshot = manager.snapshot();
    assert_eq!(runtime_snapshot.entries[0].in_flight_requests, 6);
    assert_eq!(runtime_snapshot.entries[0].max_concurrent_requests, 200);
    assert_eq!(
        runtime_snapshot.entries[0].max_concurrent_requests_override,
        Some(200)
    );

    let base_snapshot = manager.base_snapshot();
    assert_eq!(base_snapshot.entries[0].max_concurrent_requests, 200);
    assert_eq!(
        base_snapshot.entries[0].max_concurrent_requests_override,
        Some(200)
    );

    for mut lease in leases {
        lease.release_in_flight();
    }
}

#[tokio::test]
async fn test_concurrency_limiter_times_out_after_dispatch_wait_limit() {
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 1;
    config.credential_dispatch_max_wait_secs = 1;
    config.credential_in_flight_lease_max_secs = 0;

    let mut cred = KiroCredentials::default();
    cred.access_token = Some("t1".to_string());
    cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager = Arc::new(MultiTokenManager::new(config, vec![cred], None, None, false).unwrap());
    let mut first = manager.acquire_context(None).await.unwrap();

    let started = Instant::now();
    let err = manager
        .acquire_context(None)
        .await
        .err()
        .unwrap()
        .to_string();

    assert!(
        started.elapsed() >= StdDuration::from_millis(900),
        "排队等待上限生效前不应提前失败"
    );
    assert!(
        err.contains("账号调度排队等待超时"),
        "错误应提示调度排队超时，实际: {}",
        err
    );
    assert!(
        err.contains("max_wait_secs=1"),
        "错误应包含配置的等待上限，实际: {}",
        err
    );

    first.release_in_flight();
}

#[tokio::test]
async fn test_in_flight_lease_guard_drop_releases_slot() {
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 1;

    let mut cred = KiroCredentials::default();
    cred.access_token = Some("t1".to_string());
    cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();
    {
        let ctx = manager.acquire_context(None).await.unwrap();
        assert_eq!(ctx.in_flight_lease_id(), Some(1));
        let snapshot = manager.snapshot();
        assert_eq!(snapshot.entries[0].in_flight_requests, 1);
    }

    let snapshot = manager.snapshot();
    assert_eq!(snapshot.entries[0].in_flight_requests, 0);
}

#[tokio::test]
async fn test_expired_leaked_in_flight_lease_wakes_waiting_request() {
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 1;
    config.credential_in_flight_lease_max_secs = 1;

    let mut cred = KiroCredentials::default();
    cred.access_token = Some("t1".to_string());
    cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager = Arc::new(MultiTokenManager::new(config, vec![cred], None, None, false).unwrap());
    let lease = manager.acquire_in_flight_lease_for_test(1).unwrap();
    manager.age_in_flight_lease_for_test(1, lease.id(), StdDuration::from_secs(2));
    std::mem::forget(lease);

    let waiting_manager = manager.clone();
    let waiting = tokio::spawn(async move { waiting_manager.acquire_context(None).await });

    let mut ctx = tokio::time::timeout(StdDuration::from_secs(1), waiting)
        .await
        .expect("等待请求应触发超时 lease 清理并恢复调度")
        .expect("等待任务不应 panic")
        .expect("等待请求应成功获取凭据");

    assert_eq!(ctx.id, 1);
    assert_eq!(manager.snapshot().entries[0].in_flight_requests, 1);
    ctx.release_in_flight();
    assert_eq!(manager.snapshot().entries[0].in_flight_requests, 0);
}

#[tokio::test]
async fn test_manual_clear_in_flight_leases_wakes_waiting_request() {
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 1;
    config.credential_in_flight_lease_max_secs = 0;

    let mut cred = KiroCredentials::default();
    cred.access_token = Some("t1".to_string());
    cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager = Arc::new(MultiTokenManager::new(config, vec![cred], None, None, false).unwrap());
    let lease = manager.acquire_in_flight_lease_for_test(1).unwrap();
    let leaked_lease_id = lease.id();
    std::mem::forget(lease);

    let waiting_manager = manager.clone();
    let waiting = tokio::spawn(async move { waiting_manager.acquire_context(None).await });
    tokio::time::sleep(StdDuration::from_millis(50)).await;
    assert!(
        !waiting.is_finished(),
        "关闭自动回收且 lease 泄漏时，等待请求应保持排队"
    );

    manager.age_in_flight_lease_for_test(1, leaked_lease_id, StdDuration::from_secs(5));
    assert_eq!(
        manager.clear_in_flight_leases(1, Some(StdDuration::from_secs(3))),
        1
    );

    let mut ctx = tokio::time::timeout(StdDuration::from_secs(1), waiting)
        .await
        .expect("手动清理异常占用后等待请求应被唤醒")
        .expect("等待任务不应 panic")
        .expect("等待请求应成功获取凭据");
    assert_eq!(ctx.id, 1);
    ctx.release_in_flight();
}

#[tokio::test]
async fn test_expired_in_flight_lease_is_cleaned_and_dispatch_recovers() {
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 1;
    config.credential_in_flight_lease_max_secs = 1;

    let mut cred = KiroCredentials::default();
    cred.access_token = Some("t1".to_string());
    cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();
    let lease = manager.acquire_in_flight_lease_for_test(1).unwrap();
    manager.age_in_flight_lease_for_test(1, lease.id(), StdDuration::from_secs(2));
    std::mem::forget(lease);

    assert_eq!(manager.cleanup_expired_in_flight_leases(), 1);
    let mut ctx = manager.acquire_context(None).await.unwrap();
    assert_eq!(ctx.id, 1);
    ctx.release_in_flight();
}

#[tokio::test]
async fn test_summary_snapshot_cleans_expired_in_flight_lease() {
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 1;
    config.credential_in_flight_lease_max_secs = 1;

    let mut cred = KiroCredentials::default();
    cred.access_token = Some("t1".to_string());
    cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();
    let lease = manager.acquire_in_flight_lease_for_test(1).unwrap();
    manager.age_in_flight_lease_for_test(1, lease.id(), StdDuration::from_secs(2));
    std::mem::forget(lease);

    let summary = manager.summary_snapshot();
    assert_eq!(summary.global_in_flight_requests, 0);
    assert_eq!(manager.snapshot().entries[0].in_flight_requests, 0);
}

#[tokio::test]
async fn test_added_credential_warmup_does_not_fake_success_count() {
    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();
    config.credential_warmup_requests = 2;
    config.credential_warmup_selection_percent = 0;

    let mut existing = KiroCredentials::default();
    existing.access_token = Some("existing".to_string());
    existing.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager = MultiTokenManager::new(config, vec![existing], None, None, false).unwrap();

    let mut new_cred = KiroCredentials::default();
    new_cred.kiro_api_key = Some("ksk_new_key".to_string());
    new_cred.auth_method = Some("api_key".to_string());
    let new_id = manager.add_credential(new_cred).await.unwrap();

    let snapshot = manager.snapshot();
    let added = snapshot
        .entries
        .iter()
        .find(|entry| entry.id == new_id)
        .unwrap();
    assert_eq!(added.success_count, 0);
    assert_eq!(added.warmup_remaining, 2);

    let mut ctx = manager.acquire_context(None).await.unwrap();
    assert_ne!(ctx.id, new_id);
    manager.report_success(ctx.id);
    ctx.release_in_flight();

    manager.set_warmup_remaining(new_id, 0).unwrap();
    let mut ctx = manager.acquire_context(None).await.unwrap();
    assert_eq!(ctx.id, new_id);
    manager.report_success(ctx.id);
    ctx.release_in_flight();

    let snapshot = manager.snapshot();
    let added = snapshot
        .entries
        .iter()
        .find(|entry| entry.id == new_id)
        .unwrap();
    assert_eq!(added.success_count, 1);
    assert_eq!(added.warmup_remaining, 0);
}

#[tokio::test]
async fn test_warmup_selection_percent_allows_real_request_sampling() {
    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();
    config.credential_warmup_requests = 2;
    config.credential_warmup_selection_percent = 100;

    let mut ready = KiroCredentials::default();
    ready.access_token = Some("ready".to_string());
    ready.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    let mut warming = KiroCredentials::default();
    warming.access_token = Some("warming".to_string());
    warming.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager = MultiTokenManager::new(config, vec![ready, warming], None, None, false).unwrap();
    manager.set_warmup_remaining(2, 2).unwrap();

    let mut ctx = manager.acquire_context(None).await.unwrap();
    assert_eq!(ctx.id, 2);
    manager.report_success(ctx.id);
    ctx.release_in_flight();

    let snapshot = manager.snapshot();
    let warming = snapshot.entries.iter().find(|entry| entry.id == 2).unwrap();
    assert_eq!(warming.success_count, 1);
    assert_eq!(warming.warmup_remaining, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redis_backed_in_flight_limit_is_shared_between_managers() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };

    let mut config = Config::default();
    config.credential_max_concurrent_requests = 1;
    config.credential_dispatch_max_wait_secs = 2;
    let credential_id = 98_640;
    let mut credential = api_key_credential("shared-manager-in-flight");
    credential.id = Some(credential_id);

    let manager_a = Arc::new(
        MultiTokenManager::new_with_stores(
            config.clone(),
            vec![credential.clone()],
            None,
            None,
            false,
            None,
            Some(redis_store.clone()),
        )
        .unwrap(),
    );
    let manager_b = Arc::new(
        MultiTokenManager::new_with_stores(
            config,
            vec![credential],
            None,
            None,
            false,
            None,
            Some(redis_store.clone()),
        )
        .unwrap(),
    );

    let mut first = manager_a.acquire_context(None).await.unwrap();
    let waiting_manager = manager_b.clone();
    let waiting = tokio::spawn(async move { waiting_manager.acquire_context(None).await });
    tokio::time::sleep(StdDuration::from_millis(100)).await;
    assert!(
        !waiting.is_finished(),
        "另一个 manager 应看到 Redis 中的并发占用并排队"
    );

    first.release_in_flight();
    let mut second = tokio::time::timeout(StdDuration::from_secs(2), waiting)
        .await
        .expect("释放 Redis 并发槽后等待请求应恢复")
        .expect("等待任务不应 panic")
        .expect("等待请求应成功");
    second.release_in_flight();
    redis_store
        .clear_in_flight_leases(credential_id, None)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn credential_rpm_reload_replaces_old_redis_pacing_across_managers() {
    let Some(postgres_store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        postgres_store.drop_test_schema().await.unwrap();
        return;
    };

    let mut credential = api_key_credential("rpm-reload");
    credential.id = Some(1);
    credential.rpm = Some(1);
    postgres_store
        .save_credentials(&[credential.clone()])
        .await
        .unwrap();
    let manager_a = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![credential.clone()],
        None,
        None,
        false,
        Some(postgres_store.clone()),
        Some(redis_store.clone()),
    )
    .unwrap();
    let manager_b = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![credential],
        None,
        None,
        false,
        Some(postgres_store.clone()),
        Some(redis_store.clone()),
    )
    .unwrap();

    let mut first = manager_a.acquire_context(None).await.unwrap();
    first.mark_upstream_dispatch_started();
    first.release_in_flight();
    assert!(manager_a.snapshot().entries[0].rate_limited);

    manager_a.set_credential_rpm(1, Some(6_000)).unwrap();
    assert!(manager_b.reload_credentials_from_postgres().unwrap());
    let mut second = tokio::time::timeout(
        StdDuration::from_millis(500),
        manager_b.acquire_context(None),
    )
    .await
    .expect("the reloaded 6000 RPM override must replace the old Redis one-RPM deadline")
    .unwrap();
    assert_eq!(second.id, 1);
    second.mark_upstream_dispatch_started();
    second.release_in_flight();

    redis_store.clear_rate_limit(1).await.unwrap();
    redis_store.clear_in_flight_leases(1, None).await.unwrap();
    postgres_store.drop_test_schema().await.unwrap();
}

#[tokio::test]
async fn redis_admission_local_commit_failure_rolls_back_unissued_pacing_slot() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let mut credential = api_key_credential("local-commit-rollback");
    credential.id = Some(1);
    credential.rpm = Some(60);
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![credential],
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();
    let lease_id = redis_store.next_in_flight_lease_id().await.unwrap();
    let admission = redis_store
        .acquire_dispatch_lease_with_rate_limit(
            1,
            lease_id,
            1,
            1,
            1,
            60,
            Some(StdDuration::from_secs(60)),
            InFlightKind::Api.as_str(),
        )
        .await
        .unwrap();
    let reservation = match admission {
        SchedulerDispatchAdmission::Acquired {
            rate_limit_available_at_ms: Some(redis_deadline_ms),
            rate_limit_remaining_ms: Some(remaining_ms),
            rate_limit_rpm: Some(rpm),
            rate_limit_owner_lease_id: Some(owner_lease_id),
            ..
        } => RateLimitReservation {
            available_at: Instant::now() + StdDuration::from_millis(remaining_ms),
            rpm,
            owner_lease_id: Some(owner_lease_id),
            redis_deadline_ms: Some(redis_deadline_ms),
        },
        other => panic!("Redis admission should succeed, got {other:?}"),
    };

    {
        let mut entries = manager.entries.lock();
        entries[0].in_flight_requests = 1;
    }
    assert!(matches!(
        manager.acquire_local_in_flight_slot_with_id(
            1,
            lease_id,
            Instant::now(),
            1,
            1,
            1,
            0,
            Some(reservation),
            false,
        ),
        LocalInFlightSlotOutcome::ConcurrencyFull
    ));
    release_redis_in_flight_lease_and_wakeup(redis_store.clone(), 1, lease_id, false, true, 2)
        .await
        .unwrap();
    manager.entries.lock()[0].in_flight_requests = 0;

    let state = redis_store
        .scheduler_state_for_credentials(&[1])
        .await
        .unwrap()
        .remove(&1)
        .unwrap();
    assert!(state.rate_limit_available_at_ms.is_none());
    assert!(state.in_flight_leases.is_empty());
}

#[tokio::test]
async fn redis_admission_pre_eval_delay_does_not_expire_local_pacing_early() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let mut credential = api_key_credential("admission-pre-eval-delay");
    credential.id = Some(1);
    credential.rpm = Some(60);
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![credential],
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();
    redis_store.delay_next_scheduler_admission_before_eval(StdDuration::from_millis(125));

    let started_at = Instant::now();
    let mut context = manager.acquire_context(None).await.unwrap();
    let admission_elapsed = started_at.elapsed();
    let local_remaining = manager.entries.lock()[0]
        .rate_limit_available_at
        .expect("successful paced admission must install a local deadline")
        .saturating_duration_since(Instant::now());

    assert!(admission_elapsed >= StdDuration::from_millis(100));
    assert!(
        local_remaining >= StdDuration::from_millis(900),
        "delay before Redis executes the Lua script must not be deducted from its deadline: elapsed={admission_elapsed:?}, remaining={local_remaining:?}"
    );

    context.mark_upstream_dispatch_started();
    context.release_in_flight();
    redis_store.clear_rate_limit(1).await.unwrap();
    redis_store.clear_in_flight_leases(1, None).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_request_cannot_leave_committed_redis_admission() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let mut credential = api_key_credential("cancelled-admission-cleanup");
    credential.id = Some(1);
    credential.rpm = Some(60);
    let manager = Arc::new(
        MultiTokenManager::new_with_stores(
            Config::default(),
            vec![credential],
            None,
            None,
            false,
            None,
            Some(redis_store.clone()),
        )
        .unwrap(),
    );
    redis_store.delay_next_scheduler_admission_after_eval(StdDuration::from_millis(500));
    let acquiring_manager = manager.clone();
    let acquiring = tokio::spawn(async move { acquiring_manager.acquire_context(None).await });

    tokio::time::timeout(StdDuration::from_secs(2), async {
        loop {
            let state = redis_store
                .scheduler_state_for_credentials(&[1])
                .await
                .unwrap()
                .remove(&1)
                .unwrap();
            if !state.in_flight_leases.is_empty() {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("test admission never committed in Redis");

    acquiring.abort();
    let _ = acquiring.await;
    {
        let entries = manager.entries.lock();
        assert_eq!(entries[0].in_flight_requests, 0);
        assert!(entries[0].pending_redis_admission.is_none());
    }

    tokio::time::timeout(StdDuration::from_secs(3), async {
        loop {
            let state = redis_store
                .scheduler_state_for_credentials(&[1])
                .await
                .unwrap()
                .remove(&1)
                .unwrap();
            if state.in_flight_leases.is_empty() && state.rate_limit_available_at_ms.is_none() {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(20)).await;
        }
    })
    .await
    .expect("cancelled admission was not reconciled");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scheduler_admission_gate_saturation_is_fail_fast_capacity_not_redis_failure() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let credential_id = 98_701;
    redis_store.clear_rate_limit(credential_id).await.unwrap();
    redis_store
        .clear_in_flight_leases(credential_id, None)
        .await
        .unwrap();
    let mut credential = api_key_credential("admission-gate-saturation");
    credential.id = Some(credential_id);
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![credential],
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();
    let gate = manager.scheduler_redis_admission_gate.clone();
    let mut held_permits = Vec::with_capacity(SCHEDULER_REDIS_ADMISSION_MAX_IN_FLIGHT);
    for _ in 0..SCHEDULER_REDIS_ADMISSION_MAX_IN_FLIGHT {
        held_permits.push(
            gate.clone()
                .try_acquire_owned()
                .expect("test must fill every admission execution slot"),
        );
    }
    assert_eq!(gate.available_permits(), 0);
    let eval_count = redis_store.scheduler_admission_eval_count();
    let excluded = HashSet::new();

    let delayed_release_permit = held_permits.pop().unwrap();
    let delayed_release = tokio::spawn(async move {
        tokio::time::sleep(StdDuration::from_millis(10)).await;
        drop(delayed_release_permit);
    });
    let mut context = manager
        .acquire_context_for_session_with_mode(
            None,
            None,
            &excluded,
            AcquireMode::FailFastOnCapacity,
            1,
        )
        .await
        .expect("a gate permit released within 10ms must be consumed by real Redis admission");
    delayed_release.await.unwrap();
    assert_eq!(
        redis_store.scheduler_admission_eval_count(),
        eval_count + 1,
        "the bounded gate wait must absorb a short healthy wave and execute EVAL"
    );
    context.mark_upstream_dispatch_started();
    context.release_in_flight();

    held_permits.push(
        gate.clone()
            .try_acquire_owned()
            .expect("the successful EVAL must release its gate permit"),
    );
    assert_eq!(gate.available_permits(), 0);
    let saturated_eval_count = redis_store.scheduler_admission_eval_count();
    let started_at = Instant::now();
    let error = manager
        .acquire_context_for_session_with_mode(
            None,
            None,
            &excluded,
            AcquireMode::FailFastOnCapacity,
            1,
        )
        .await
        .err()
        .expect("a saturated admission gate must fail fast as capacity");

    assert!(
        started_at.elapsed()
            <= SCHEDULER_REDIS_ADMISSION_GATE_WAIT_MAX + StdDuration::from_millis(25),
        "sustained gate pressure must route as capacity within 75ms"
    );
    assert!(error.to_string().contains("调度容量暂不可用"));
    assert!(!error.to_string().contains("Redis 调度协调状态不可用"));
    assert_eq!(
        redis_store.scheduler_admission_eval_count(),
        saturated_eval_count
    );
    assert_eq!(
        manager
            .scheduler_redis_admission_failure_streak
            .load(Ordering::Acquire),
        0,
        "execution-slot pressure must not count as a Redis health failure"
    );
    assert!(manager.scheduler_redis_degraded_until.lock().is_none());
    drop(held_permits);
    redis_store.clear_rate_limit(credential_id).await.unwrap();
    redis_store
        .clear_in_flight_leases(credential_id, None)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn provisional_redis_admission_prevents_forty_account_reselection_wave() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let credentials = (0..40)
        .map(|offset| {
            let mut credential = api_key_credential(&format!("provisional-wave-{offset}"));
            credential.id = Some(99_100 + offset);
            credential.rpm = Some(60);
            credential
        })
        .collect();
    let manager = Arc::new(
        MultiTokenManager::new_with_stores(
            Config::default(),
            credentials,
            None,
            None,
            false,
            None,
            Some(redis_store.clone()),
        )
        .unwrap(),
    );
    *manager.last_scheduler_redis_sync_at.lock() = Some(Instant::now());
    manager.delay_each_redis_admission_owner_before_submit(StdDuration::from_millis(70));
    let eval_count = redis_store.scheduler_admission_eval_count();
    let barrier = Arc::new(tokio::sync::Barrier::new(41));
    let mut tasks = Vec::new();
    for _ in 0..40 {
        let task_manager = manager.clone();
        let task_barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            task_barrier.wait().await;
            task_manager
                .acquire_context_for_session_with_mode(
                    None,
                    None,
                    &HashSet::new(),
                    AcquireMode::SelectiveFailFast {
                        rate_limit: true,
                        concurrency: true,
                    },
                    1,
                )
                .await
        }));
    }
    barrier.wait().await;

    let mut contexts = Vec::new();
    for result in futures::future::join_all(tasks).await {
        contexts.push(
            result
                .expect("provisional admission task must not panic")
                .expect("40 credentials must admit the 40-request wave"),
        );
    }
    let selected_ids = contexts
        .iter()
        .map(|context| context.id)
        .collect::<HashSet<_>>();
    assert_eq!(selected_ids.len(), 40);
    assert_eq!(
        redis_store.scheduler_admission_eval_count(),
        eval_count + 40,
        "each request must submit exactly one admission EVAL"
    );
    assert_eq!(
        manager
            .scheduler_redis_admission_failure_streak
            .load(Ordering::Acquire),
        0
    );
    assert!(
        manager
            .entries
            .lock()
            .iter()
            .all(|entry| entry.pending_redis_admission.is_none())
    );
    for context in &mut contexts {
        context.mark_upstream_dispatch_started();
        context.release_in_flight();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn provisional_redis_admission_preserves_no_rpm_concurrency() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let credential_id = 99_149;
    let mut credential = api_key_credential("provisional-no-rpm");
    credential.id = Some(credential_id);
    credential.rpm = Some(0);
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 20;
    let manager = Arc::new(
        MultiTokenManager::new_with_stores(
            config,
            vec![credential],
            None,
            None,
            false,
            None,
            Some(redis_store.clone()),
        )
        .unwrap(),
    );
    *manager.last_scheduler_redis_sync_at.lock() = Some(Instant::now());
    manager.delay_each_redis_admission_owner_before_submit(StdDuration::from_millis(70));
    let eval_count = redis_store.scheduler_admission_eval_count();
    let barrier = Arc::new(tokio::sync::Barrier::new(21));
    let mut tasks = Vec::new();
    for _ in 0..20 {
        let task_manager = manager.clone();
        let task_barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            task_barrier.wait().await;
            task_manager
                .acquire_context_for_session_with_mode(
                    None,
                    None,
                    &HashSet::new(),
                    AcquireMode::SelectiveFailFast {
                        rate_limit: true,
                        concurrency: true,
                    },
                    1,
                )
                .await
        }));
    }
    barrier.wait().await;

    let mut contexts = Vec::new();
    for result in futures::future::join_all(tasks).await {
        contexts.push(
            result
                .expect("no-RPM admission task must not panic")
                .expect("one credential must fill all 20 configured concurrency slots"),
        );
    }
    assert_eq!(manager.entries.lock()[0].in_flight_requests, 20);
    assert!(manager.entries.lock()[0].pending_redis_admission.is_none());
    assert_eq!(
        redis_store.scheduler_admission_eval_count(),
        eval_count + 20
    );
    for context in &mut contexts {
        context.mark_upstream_dispatch_started();
        context.release_in_flight();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_redis_pacing_survives_snapshot_and_rejects_second_eval_immediately() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let credential_id = 99_150;
    let mut credential = api_key_credential("pending-pacing-single");
    credential.id = Some(credential_id);
    credential.rpm = Some(60);
    let manager = Arc::new(
        MultiTokenManager::new_with_stores(
            Config::default(),
            vec![credential],
            None,
            None,
            false,
            None,
            Some(redis_store.clone()),
        )
        .unwrap(),
    );
    *manager.last_scheduler_redis_sync_at.lock() = Some(Instant::now());
    manager.delay_next_redis_admission_owner_before_submit(StdDuration::from_millis(100));
    let eval_count = redis_store.scheduler_admission_eval_count();
    let first_manager = manager.clone();
    let first = tokio::spawn(async move { first_manager.acquire_context(None).await });

    tokio::time::timeout(StdDuration::from_secs(1), async {
        loop {
            if manager.entries.lock()[0].pending_redis_admission.is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first request must install provisional pacing before awaiting Redis");

    manager.apply_scheduler_states(HashMap::from([(
        credential_id,
        SchedulerCredentialState::default(),
    )]));
    {
        let entries = manager.entries.lock();
        assert_eq!(entries[0].in_flight_requests, 1);
        assert!(entries[0].pending_redis_admission.is_some());
    }

    let before_second_eval = redis_store.scheduler_admission_eval_count();
    let started_at = Instant::now();
    let error = manager
        .acquire_context_for_session_with_mode(
            None,
            None,
            &HashSet::new(),
            AcquireMode::SelectiveFailFast {
                rate_limit: true,
                concurrency: true,
            },
            1,
        )
        .await
        .err()
        .expect("a second request must use the external RPM fallback signal");
    assert!(started_at.elapsed() < StdDuration::from_millis(75));
    assert!(error.to_string().contains("RPM 调度暂不可用"));
    assert_eq!(
        redis_store.scheduler_admission_eval_count(),
        before_second_eval,
        "pending local pacing must suppress a duplicate Redis EVAL"
    );

    let mut first_context = tokio::time::timeout(StdDuration::from_secs(1), first)
        .await
        .expect("first admission must finish")
        .expect("first admission task must not panic")
        .expect("first admission must succeed");
    assert_eq!(redis_store.scheduler_admission_eval_count(), eval_count + 1);
    assert!(manager.entries.lock()[0].pending_redis_admission.is_none());
    first_context.mark_upstream_dispatch_started();
    first_context.release_in_flight();
}

#[tokio::test(flavor = "current_thread")]
async fn admin_runtime_snapshots_share_background_redis_refresh_and_report_real_freshness() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let credentials = (0..40)
        .map(|offset| {
            let mut credential = api_key_credential(&format!("runtime-singleflight-{offset}"));
            credential.id = Some(99_200 + offset);
            credential
        })
        .collect();
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        credentials,
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();
    let ids = (0..40).map(|offset| 99_200 + offset).collect::<Vec<_>>();
    let old_success = Instant::now()
        .checked_sub(SCHEDULER_REDIS_RUNTIME_FRESH_MAX_AGE + StdDuration::from_secs(1))
        .unwrap();
    *manager.last_scheduler_redis_sync_success_at.lock() = Some(old_success);
    *manager.last_scheduler_redis_sync_at.lock() = None;
    let snapshot_count = redis_store.scheduler_state_snapshot_count();

    for _ in 0..10 {
        let snapshot = manager.runtime_snapshot_for_ids(&ids);
        assert!(!snapshot.runtime_fresh);
    }
    assert_eq!(
        redis_store.scheduler_state_snapshot_count(),
        snapshot_count,
        "Admin reads must enqueue one background refresh without blocking for it"
    );

    tokio::time::timeout(StdDuration::from_secs(1), async {
        loop {
            if redis_store.scheduler_state_snapshot_count() == snapshot_count + 1
                && manager.scheduler_redis_runtime_fresh(Instant::now())
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the shared background Redis snapshot must complete");
    assert!(manager.runtime_snapshot_for_ids(&ids).runtime_fresh);
    assert_eq!(
        redis_store.scheduler_state_snapshot_count(),
        snapshot_count + 1,
        "repeated Admin reads inside the one-second interval must reuse the same snapshot"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scheduler_admission_gate_busy_is_pool_scoped_across_forty_credentials() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let credentials = (0..40)
        .map(|offset| {
            let mut credential = api_key_credential(&format!("gate-busy-{offset}"));
            credential.id = Some(98_710 + offset);
            credential
        })
        .collect();
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        credentials,
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();
    let gate = manager.scheduler_redis_admission_gate.clone();
    let held_permits = (0..SCHEDULER_REDIS_ADMISSION_MAX_IN_FLIGHT)
        .map(|_| {
            gate.clone()
                .try_acquire_owned()
                .expect("test must fill every admission execution slot")
        })
        .collect::<Vec<_>>();
    let eval_count = redis_store.scheduler_admission_eval_count();

    let started_at = Instant::now();
    let error = manager
        .acquire_context_for_session_with_mode(
            None,
            None,
            &HashSet::new(),
            AcquireMode::SelectiveFailFast {
                rate_limit: true,
                concurrency: true,
            },
            1,
        )
        .await
        .err()
        .expect("a pool-scoped busy gate must fail external preflight");

    assert!(
        started_at.elapsed()
            <= SCHEDULER_REDIS_ADMISSION_GATE_WAIT_MAX + StdDuration::from_millis(25),
        "gate busy must be paid once for the pool, not once per credential"
    );
    assert!(error.to_string().contains("调度容量暂不可用"));
    assert!(!error.to_string().contains("Redis 调度协调状态不可用"));
    assert_eq!(redis_store.scheduler_admission_eval_count(), eval_count);
    assert_eq!(
        manager
            .scheduler_redis_admission_failure_streak
            .load(Ordering::Acquire),
        0
    );
    drop(held_permits);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redis_global_capacity_full_is_pool_scoped_without_account_reselection() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let credential_ids = (0..40).map(|offset| 98_800 + offset).collect::<Vec<_>>();
    let credentials = credential_ids
        .iter()
        .copied()
        .map(|id| {
            let mut credential = api_key_credential(&format!("global-full-{id}"));
            credential.id = Some(id);
            credential
        })
        .collect();
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 15;
    config.dispatch_global_max_concurrent_requests = 1;
    let manager = MultiTokenManager::new_with_stores(
        config,
        credentials,
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();
    *manager.last_scheduler_redis_sync_at.lock() = Some(Instant::now());
    let occupied =
        occupy_redis_dispatch_slots(&redis_store, &credential_ids[..1], 15, 1, 9_880_000).await;
    let eval_count = redis_store.scheduler_admission_eval_count();

    let error = manager
        .acquire_context_for_session_with_mode(
            None,
            None,
            &HashSet::new(),
            AcquireMode::SelectiveFailFast {
                rate_limit: false,
                concurrency: true,
            },
            1,
        )
        .await
        .err()
        .expect("global capacity must fail external preflight");

    assert!(error.to_string().contains("调度容量暂不可用"));
    assert_eq!(
        redis_store.scheduler_admission_eval_count(),
        eval_count + 1,
        "authoritative global capacity rejection must not walk every credential"
    );
    release_redis_dispatch_slots(&redis_store, &occupied).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_mode_global_redis_capacity_full_queues_after_one_rejection_eval() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let credential_ids = (0..40).map(|offset| 98_850 + offset).collect::<Vec<_>>();
    let credentials = credential_ids
        .iter()
        .copied()
        .map(|id| {
            let mut credential = api_key_credential(&format!("wait-global-full-{id}"));
            credential.id = Some(id);
            credential
        })
        .collect();
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 15;
    config.dispatch_global_max_concurrent_requests = 1;
    config.dispatch_max_queued_requests = 10;
    let manager = Arc::new(
        MultiTokenManager::new_with_stores(
            config,
            credentials,
            None,
            None,
            false,
            None,
            Some(redis_store.clone()),
        )
        .unwrap(),
    );
    let occupied =
        occupy_redis_dispatch_slots(&redis_store, &credential_ids[..1], 15, 1, 9_885_000).await;
    *manager.last_scheduler_redis_sync_at.lock() = Some(Instant::now());
    let eval_count = redis_store.scheduler_admission_eval_count();
    let waiting_manager = manager.clone();
    let waiting = tokio::spawn(async move {
        waiting_manager
            .acquire_context_for_session_with_mode(
                None,
                None,
                &HashSet::new(),
                AcquireMode::WaitForCapacityMax(StdDuration::from_secs(2)),
                1,
            )
            .await
    });

    tokio::time::timeout(StdDuration::from_secs(1), async {
        loop {
            if manager.snapshot().queued_requests == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("global Redis capacity rejection must enter the wait queue");
    assert!(!waiting.is_finished());
    assert_eq!(
        redis_store.scheduler_admission_eval_count(),
        eval_count + 1,
        "global capacity code 4 must queue after one pool-scoped EVAL instead of walking accounts"
    );

    release_redis_dispatch_slots(&redis_store, &occupied).await;
    manager.in_flight_notify.notify_one();
    let mut context = tokio::time::timeout(StdDuration::from_secs(1), waiting)
        .await
        .expect("global capacity waiter must resume after the Redis lease is released")
        .expect("global capacity waiter task must not panic")
        .expect("global capacity waiter must acquire after capacity returns");
    assert_eq!(
        redis_store.scheduler_admission_eval_count(),
        eval_count + 2,
        "capacity recovery should need only the original rejection and one successful EVAL"
    );
    assert_eq!(manager.snapshot().queued_requests, 0);
    context.mark_upstream_dispatch_started();
    context.release_in_flight();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn credential_scoped_redis_capacity_can_reselect_a_later_free_credential() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let credential_ids = (0..40).map(|offset| 98_900 + offset).collect::<Vec<_>>();
    let credentials = credential_ids
        .iter()
        .copied()
        .map(|id| {
            let mut credential = api_key_credential(&format!("credential-full-{id}"));
            credential.id = Some(id);
            credential
        })
        .collect();
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 1;
    config.dispatch_global_max_concurrent_requests = 0;
    let manager = MultiTokenManager::new_with_stores(
        config,
        credentials,
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();
    *manager.last_scheduler_redis_sync_at.lock() = Some(Instant::now());
    let occupied =
        occupy_redis_dispatch_slots(&redis_store, &credential_ids[..39], 1, 0, 9_890_000).await;

    let mut context = manager
        .acquire_context_for_session_with_mode(
            None,
            None,
            &HashSet::new(),
            AcquireMode::SelectiveFailFast {
                rate_limit: false,
                concurrency: true,
            },
            1,
        )
        .await
        .expect("credential-scoped capacity rejection must preserve account reselection");

    assert_eq!(context.id, credential_ids[39]);
    context.mark_upstream_dispatch_started();
    context.release_in_flight();
    release_redis_dispatch_slots(&redis_store, &occupied).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_mode_reselects_a_free_lower_priority_credential_without_queueing() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let credential_ids = [98_950, 98_951];
    let mut preferred = api_key_credential("wait-preferred-redis-full");
    preferred.id = Some(credential_ids[0]);
    preferred.priority = 0;
    let mut fallback = api_key_credential("wait-fallback-redis-free");
    fallback.id = Some(credential_ids[1]);
    fallback.priority = 1;
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 1;
    config.dispatch_max_queued_requests = 10;
    let manager = MultiTokenManager::new_with_stores(
        config,
        vec![preferred, fallback],
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();
    let occupied =
        occupy_redis_dispatch_slots(&redis_store, &credential_ids[..1], 1, 0, 9_895_000).await;
    *manager.last_scheduler_redis_sync_at.lock() = Some(Instant::now());
    let eval_count = redis_store.scheduler_admission_eval_count();

    let started_at = Instant::now();
    let mut context = tokio::time::timeout(
        StdDuration::from_secs(1),
        manager.acquire_context_for_session_with_mode(
            None,
            None,
            &HashSet::new(),
            AcquireMode::WaitForCapacityMax(StdDuration::from_secs(1)),
            1,
        ),
    )
    .await
    .expect("Wait mode must not queue behind a Redis-full preferred credential")
    .expect("Wait mode must immediately reselect the free credential");

    assert_eq!(context.id, credential_ids[1]);
    assert!(
        started_at.elapsed() <= SCHEDULER_REDIS_RESELECTION_BUDGET + StdDuration::from_millis(75),
        "account-scoped Redis capacity rejection must stay inside one reselection wave"
    );
    assert_eq!(
        redis_store.scheduler_admission_eval_count(),
        eval_count + 2,
        "Wait mode should evaluate the full preferred account once and the free fallback once"
    );
    assert_eq!(manager.snapshot().queued_requests, 0);
    context.mark_upstream_dispatch_started();
    context.release_in_flight();
    release_redis_dispatch_slots(&redis_store, &occupied).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_mode_restores_redis_capacity_exclusions_before_queueing() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let credential_ids = [98_960, 98_961];
    let credentials = credential_ids
        .iter()
        .copied()
        .enumerate()
        .map(|(priority, id)| {
            let mut credential = api_key_credential(&format!("wait-restore-full-{id}"));
            credential.id = Some(id);
            credential.priority = priority as u32;
            credential
        })
        .collect();
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 1;
    config.dispatch_max_queued_requests = 10;
    let manager = Arc::new(
        MultiTokenManager::new_with_stores(
            config,
            credentials,
            None,
            None,
            false,
            None,
            Some(redis_store.clone()),
        )
        .unwrap(),
    );
    let occupied =
        occupy_redis_dispatch_slots(&redis_store, &credential_ids, 1, 0, 9_896_000).await;
    *manager.last_scheduler_redis_sync_at.lock() = Some(Instant::now());
    let eval_count = redis_store.scheduler_admission_eval_count();
    let waiting_manager = manager.clone();
    let waiting = tokio::spawn(async move {
        waiting_manager
            .acquire_context_for_session_with_mode(
                None,
                None,
                &HashSet::new(),
                AcquireMode::WaitForCapacityMax(StdDuration::from_secs(2)),
                1,
            )
            .await
    });

    tokio::time::timeout(StdDuration::from_secs(1), async {
        loop {
            if manager.snapshot().queued_requests == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("exhausting account-scoped Redis candidates must wait instead of returning an exclusion error");
    assert!(!waiting.is_finished());
    assert_eq!(
        redis_store.scheduler_admission_eval_count(),
        eval_count + 2,
        "one reselection wave should inspect each account once before waiting"
    );

    release_redis_dispatch_slots(&redis_store, &occupied[1..]).await;
    manager.in_flight_notify.notify_one();
    let mut context = tokio::time::timeout(StdDuration::from_secs(1), waiting)
        .await
        .expect("account capacity waiter must resume after one Redis lease is released")
        .expect("account capacity waiter task must not panic")
        .expect("restored candidates must be eligible after real waiting");
    assert_eq!(context.id, credential_ids[1]);
    assert_eq!(manager.snapshot().queued_requests, 0);
    context.mark_upstream_dispatch_started();
    context.release_in_flight();
    release_redis_dispatch_slots(&redis_store, &occupied[..1]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_credential_rejections_share_one_fail_fast_reselection_deadline() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let credential_ids = (0..40).map(|offset| 99_000 + offset).collect::<Vec<_>>();
    let credentials = credential_ids
        .iter()
        .copied()
        .map(|id| {
            let mut credential = api_key_credential(&format!("slow-capacity-{id}"));
            credential.id = Some(id);
            credential
        })
        .collect();
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 1;
    config.dispatch_global_max_concurrent_requests = 0;
    let manager = MultiTokenManager::new_with_stores(
        config,
        credentials,
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();
    *manager.last_scheduler_redis_sync_at.lock() = Some(Instant::now());
    let occupied =
        occupy_redis_dispatch_slots(&redis_store, &credential_ids, 1, 0, 9_900_000).await;
    manager.delay_each_redis_admission_owner_before_submit(StdDuration::from_millis(70));
    let eval_count = redis_store.scheduler_admission_eval_count();

    let started_at = Instant::now();
    let error = manager
        .acquire_context_for_session_with_mode(
            None,
            None,
            &HashSet::new(),
            AcquireMode::SelectiveFailFast {
                rate_limit: false,
                concurrency: true,
            },
            1,
        )
        .await
        .err()
        .expect("all credential capacity rejections must fail external preflight");
    let elapsed = started_at.elapsed();
    let eval_delta = redis_store.scheduler_admission_eval_count() - eval_count;

    assert!(
        elapsed <= SCHEDULER_REDIS_RESELECTION_BUDGET + StdDuration::from_millis(75),
        "40 account delays must share one deadline, elapsed={elapsed:?}"
    );
    assert!(
        eval_delta <= 4,
        "the shared deadline must stop account walking, eval_delta={eval_delta}"
    );
    assert!(error.to_string().contains("调度容量暂不可用"));
    assert!(!error.to_string().contains("Redis 调度协调状态不可用"));
    release_redis_dispatch_slots(&redis_store, &occupied).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scheduler_admission_owner_past_deadline_never_submits_eval() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let mut credential = api_key_credential("admission-owner-deadline");
    credential.id = Some(98_702);
    credential.rpm = Some(60);
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![credential],
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();
    manager.delay_next_redis_admission_owner_before_submit(
        SCHEDULER_REDIS_ADMISSION_OP_TIMEOUT + StdDuration::from_millis(100),
    );
    let eval_count = redis_store.scheduler_admission_eval_count();
    let excluded = HashSet::new();

    let error = manager
        .acquire_context_for_session_with_mode(
            None,
            None,
            &excluded,
            AcquireMode::FailFastOnCapacity,
            1,
        )
        .await
        .err()
        .expect("an owner that misses the absolute deadline must fail closed");
    tokio::time::sleep(StdDuration::from_millis(150)).await;

    assert!(error.to_string().contains("调度容量暂不可用"));
    assert!(!error.to_string().contains("Redis 调度协调状态不可用"));
    assert_eq!(
        redis_store.scheduler_admission_eval_count(),
        eval_count,
        "a late detached owner must observe cancellation and never submit EVAL"
    );
    assert_eq!(
        manager
            .scheduler_redis_admission_failure_streak
            .load(Ordering::Acquire),
        0,
        "an admission that never reached Redis must not advance the breaker"
    );
    assert!(manager.scheduler_redis_degraded_until.lock().is_none());
    {
        let entries = manager.entries.lock();
        assert_eq!(entries[0].in_flight_requests, 0);
        assert!(entries[0].pending_redis_admission.is_none());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redis_admission_hard_timeout_returns_before_async_cleanup_finishes() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    redis_store.clear_rate_limit(1).await.unwrap();
    redis_store.clear_in_flight_leases(1, None).await.unwrap();
    let mut credential = api_key_credential("admission-hard-timeout");
    credential.id = Some(1);
    credential.rpm = Some(60);
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![credential],
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();
    redis_store.delay_next_scheduler_admission_after_eval(StdDuration::from_millis(500));

    let started_at = Instant::now();
    let error = manager
        .acquire_context(None)
        .await
        .err()
        .expect("250ms Redis admission deadline must fail closed");
    assert!(error.to_string().contains("Redis 调度协调状态不可用"));
    assert!(
        started_at.elapsed() < StdDuration::from_secs(1),
        "request path must return independently of detached cleanup"
    );

    tokio::time::timeout(StdDuration::from_secs(3), async {
        loop {
            let state = redis_store
                .scheduler_state_for_credentials(&[1])
                .await
                .unwrap()
                .remove(&1)
                .unwrap();
            if state.in_flight_leases.is_empty() && state.rate_limit_available_at_ms.is_none() {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(20)).await;
        }
    })
    .await
    .expect("timed-out admission was not reconciled");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_caller_during_admission_commit_ack_rolls_back_redis() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    redis_store.clear_rate_limit(1).await.unwrap();
    redis_store.clear_in_flight_leases(1, None).await.unwrap();
    let mut credential = api_key_credential("admission-commit-ack-cancel");
    credential.id = Some(1);
    credential.rpm = Some(60);
    let manager = Arc::new(
        MultiTokenManager::new_with_stores(
            Config::default(),
            vec![credential],
            None,
            None,
            false,
            None,
            Some(redis_store.clone()),
        )
        .unwrap(),
    );
    manager.delay_next_redis_admission_before_local_commit(StdDuration::from_secs(5));
    let acquiring_manager = manager.clone();
    let acquiring = tokio::spawn(async move { acquiring_manager.acquire_context(None).await });

    tokio::time::timeout(StdDuration::from_secs(2), async {
        loop {
            let state = redis_store
                .scheduler_state_for_credentials(&[1])
                .await
                .unwrap()
                .remove(&1)
                .unwrap();
            if !state.in_flight_leases.is_empty() {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("admission never reached commit-ack handoff");
    acquiring.abort();
    let _ = acquiring.await;

    tokio::time::timeout(StdDuration::from_secs(3), async {
        loop {
            let state = redis_store
                .scheduler_state_for_credentials(&[1])
                .await
                .unwrap()
                .remove(&1)
                .unwrap();
            if state.in_flight_leases.is_empty() && state.rate_limit_available_at_ms.is_none() {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(20)).await;
        }
    })
    .await
    .expect("cancelled commit-ack handoff was not reconciled");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_commit_race_nacks_redis_admission_without_blocking_caller() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    redis_store.clear_rate_limit(1).await.unwrap();
    redis_store.clear_in_flight_leases(1, None).await.unwrap();
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 1;
    let mut credential = api_key_credential("admission-local-commit-race");
    credential.id = Some(1);
    credential.rpm = Some(60);
    let manager = Arc::new(
        MultiTokenManager::new_with_stores(
            config,
            vec![credential],
            None,
            None,
            false,
            None,
            Some(redis_store.clone()),
        )
        .unwrap(),
    );
    manager.delay_next_redis_admission_before_local_commit(StdDuration::from_millis(200));
    let acquiring_manager = manager.clone();
    let acquiring =
        tokio::spawn(async move { acquiring_manager.acquire_in_flight_slot(1, 1).await });

    tokio::time::timeout(StdDuration::from_secs(2), async {
        loop {
            let state = redis_store
                .scheduler_state_for_credentials(&[1])
                .await
                .unwrap()
                .remove(&1)
                .unwrap();
            if !state.in_flight_leases.is_empty() {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("admission never reached local commit handoff");
    tokio::time::timeout(StdDuration::from_secs(1), async {
        while manager.in_flight_requests_for_test(1) < 1 {
            tokio::time::sleep(StdDuration::from_millis(5)).await;
        }
    })
    .await
    .expect("local provisional reservation was not established");
    {
        let mut entries = manager.entries.lock();
        entries[0].in_flight_requests = entries[0].in_flight_requests.saturating_add(1);
    }

    let outcome = tokio::time::timeout(StdDuration::from_secs(1), acquiring)
        .await
        .expect("local commit race must return before async Redis cleanup")
        .unwrap()
        .unwrap();
    assert!(matches!(outcome, InFlightSlotOutcome::ConcurrencyFull));
    manager.entries.lock()[0].in_flight_requests = 0;

    tokio::time::timeout(StdDuration::from_secs(3), async {
        loop {
            let state = redis_store
                .scheduler_state_for_credentials(&[1])
                .await
                .unwrap()
                .remove(&1)
                .unwrap();
            if state.in_flight_leases.is_empty() && state.rate_limit_available_at_ms.is_none() {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(20)).await;
        }
    })
    .await
    .expect("local commit NACK was not reconciled");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn redis_backed_in_flight_limit_does_not_fail_open_under_concurrency() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };

    let mut config = Config::default();
    config.credential_max_concurrent_requests = 1;
    let manager_a = Arc::new(
        MultiTokenManager::new_with_stores(
            config.clone(),
            vec![api_key_credential("concurrent")],
            None,
            None,
            false,
            None,
            Some(redis_store.clone()),
        )
        .unwrap(),
    );
    let manager_b = Arc::new(
        MultiTokenManager::new_with_stores(
            config,
            vec![api_key_credential("concurrent")],
            None,
            None,
            false,
            None,
            Some(redis_store),
        )
        .unwrap(),
    );

    const CONTENDERS: usize = 8;
    let barrier = Arc::new(tokio::sync::Barrier::new(CONTENDERS));
    let mut tasks = Vec::with_capacity(CONTENDERS);
    for index in 0..CONTENDERS {
        let manager = if index % 2 == 0 {
            manager_a.clone()
        } else {
            manager_b.clone()
        };
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            manager.acquire_in_flight_slot(1, 1).await.unwrap()
        }));
    }

    let mut leases = Vec::with_capacity(CONTENDERS);
    for task in tasks {
        leases.push(task.await.expect("并发 lease 任务不应 panic"));
    }
    assert_eq!(
        leases
            .iter()
            .filter(|lease| matches!(lease, InFlightSlotOutcome::Acquired(_)))
            .count(),
        1,
        "健康 Redis 下超过两个并发请求也必须严格执行跨 manager 的单槽限制"
    );
    drop(leases);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redis_backed_in_flight_limit_does_not_fail_open_while_degraded() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };

    let mut config = Config::default();
    config.credential_max_concurrent_requests = 1;
    let manager = MultiTokenManager::new_with_stores(
        config,
        vec![api_key_credential("degraded")],
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();
    *manager.scheduler_redis_degraded_until.lock() =
        Some(Instant::now() + StdDuration::from_secs(5));
    manager.entries.lock()[0].cooldown_until = Some(Instant::now() + StdDuration::from_secs(60));
    let route_state = manager.local_pool_route_state(None);
    assert_eq!(
        route_state.kind,
        LocalPoolRouteStateKind::SchedulerRedisDegraded
    );
    assert!(
        route_state.retry_after_secs.is_some_and(|secs| secs <= 5),
        "Redis degradation must report its own backoff instead of an unrelated credential cooldown: {route_state:?}"
    );
    manager.entries.lock()[0].disabled = true;
    let disabled_route_state = manager.compute_local_pool_route_state(None);
    assert_eq!(
        disabled_route_state.kind,
        LocalPoolRouteStateKind::AllDisabled,
        "authoritatively disabled credentials should keep their independent fallback policy"
    );
    manager.entries.lock()[0].runtime_persistence_degraded = true;
    let quarantined_route_state = manager.compute_local_pool_route_state(None);
    assert_eq!(
        quarantined_route_state.kind,
        LocalPoolRouteStateKind::SchedulerRedisDegraded,
        "Redis admission degradation must not be masked as local_all_disabled"
    );
    {
        let mut entries = manager.entries.lock();
        entries[0].runtime_persistence_degraded = false;
        entries[0].disabled = false;
        entries[0].cooldown_until = None;
    }

    let queue_error = manager
        .acquire_context(None)
        .await
        .err()
        .expect("Redis 退避窗口内应拒绝分布式调度准入")
        .to_string();
    assert!(
        queue_error.contains("Redis 调度协调状态不可用"),
        "Redis 退避不应误报为等待队列已满，实际: {queue_error}"
    );
    assert!(queue_error.contains("retry_after_secs="));
    let selection_failure =
        manager.selection_failure_summary("redis-degraded", "local_account", None, &queue_error);
    assert_eq!(
        selection_failure.stage,
        SelectionFailureStage::DispatchQueue
    );
    assert_eq!(
        selection_failure.primary_reason,
        AccountRejectReason::Unknown
    );
    assert_eq!(manager.queued_requests.load(Ordering::Acquire), 0);
    assert_eq!(
        redis_store
            .global_capacity_state()
            .await
            .unwrap()
            .queued_requests,
        0,
        "跳过 Redis queue admission 时不应创建全局排队占位"
    );
    let lease_error = manager
        .acquire_in_flight_slot(1, 1)
        .await
        .expect_err("Redis 退避窗口内应明确拒绝分布式 lease 准入")
        .to_string();
    assert!(lease_error.contains("Redis 调度协调状态不可用"));
    assert_eq!(manager.entries.lock()[0].in_flight_requests, 0);
    let state = redis_store
        .scheduler_state_for_credentials(&[1])
        .await
        .unwrap();
    assert_eq!(
        state
            .get(&1)
            .map(|state| state.in_flight_leases.len())
            .unwrap_or_default(),
        0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redis_dispatch_queue_waiter_fails_closed_after_coordination_degrades() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };

    let mut config = Config::default();
    config.credential_max_concurrent_requests = 1;
    config.dispatch_global_max_concurrent_requests = 1;
    config.dispatch_max_queued_requests = 1;
    config.credential_dispatch_max_wait_secs = 0;
    let manager = Arc::new(
        MultiTokenManager::new_with_stores(
            config,
            vec![api_key_credential("degraded-after-queue-admission")],
            None,
            None,
            false,
            None,
            Some(redis_store.clone()),
        )
        .unwrap(),
    );

    let mut first = manager.acquire_context(None).await.unwrap();
    let waiting_manager = manager.clone();
    let waiting = tokio::spawn(async move { waiting_manager.acquire_context(None).await });

    tokio::time::timeout(StdDuration::from_secs(2), async {
        loop {
            if manager.queued_requests.load(Ordering::Acquire) == 1
                && redis_store
                    .global_capacity_state()
                    .await
                    .unwrap()
                    .queued_requests
                    == 1
            {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("waiter should enter the queue before Redis becomes degraded");

    *manager.scheduler_redis_degraded_until.lock() =
        Some(Instant::now() + StdDuration::from_secs(5));
    first.release_in_flight();

    let error = tokio::time::timeout(StdDuration::from_secs(2), waiting)
        .await
        .expect("queued waiter should fail closed without waiting indefinitely")
        .expect("queued waiter task should not panic")
        .err()
        .expect("queued waiter should return a coordination error")
        .to_string();
    assert!(error.contains("Redis 调度协调状态不可用"));

    tokio::time::timeout(StdDuration::from_secs(2), async {
        loop {
            if manager.queued_requests.load(Ordering::Acquire) == 0
                && redis_store
                    .global_capacity_state()
                    .await
                    .unwrap()
                    .queued_requests
                    == 0
            {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("failed-closed waiter should release its queue lease");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redis_dispatch_queue_cancelled_waiter_releases_local_and_remote_lease() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };

    let mut config = Config::default();
    config.credential_max_concurrent_requests = 1;
    config.dispatch_global_max_concurrent_requests = 1;
    config.dispatch_max_queued_requests = 1;
    config.credential_dispatch_max_wait_secs = 5;
    let manager = Arc::new(
        MultiTokenManager::new_with_stores(
            config,
            vec![api_key_credential("cancelled-queue-waiter")],
            None,
            None,
            false,
            None,
            Some(redis_store.clone()),
        )
        .unwrap(),
    );

    let mut first = manager.acquire_context(None).await.unwrap();
    let waiting_manager = manager.clone();
    let waiting = tokio::spawn(async move { waiting_manager.acquire_context(None).await });

    tokio::time::timeout(StdDuration::from_secs(2), async {
        loop {
            let remote_queued = redis_store
                .global_capacity_state()
                .await
                .unwrap()
                .queued_requests;
            if manager.queued_requests.load(Ordering::Acquire) == 1 && remote_queued == 1 {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("waiter should enter both local and Redis dispatch queues");

    waiting.abort();
    let join_error = match waiting.await {
        Err(error) => error,
        Ok(_) => panic!("aborted queue waiter should not complete normally"),
    };
    assert!(join_error.is_cancelled());
    tokio::time::timeout(StdDuration::from_secs(2), async {
        loop {
            let remote_queued = redis_store
                .global_capacity_state()
                .await
                .unwrap()
                .queued_requests;
            if manager.queued_requests.load(Ordering::Acquire) == 0 && remote_queued == 0 {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("cancelled waiter should release its queue lease without waiting for TTL");

    first.release_in_flight();
    let mut next = manager.acquire_context(None).await.unwrap();
    next.release_in_flight();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redis_backed_session_binding_and_cooldown_are_shared_between_managers() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };

    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();
    config.credential_transient_cooldown_secs = 60;

    let manager_a = MultiTokenManager::new_with_stores(
        config.clone(),
        vec![api_key_credential("a"), api_key_credential("b")],
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();
    let manager_b = MultiTokenManager::new_with_stores(
        config,
        vec![api_key_credential("a"), api_key_credential("b")],
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();
    let empty = HashSet::new();

    let mut first = manager_a
        .acquire_context_for_session(None, Some("shared-session"), &empty)
        .await
        .unwrap();
    let first_id = first.id;
    first.release_in_flight();

    tokio::time::timeout(StdDuration::from_secs(5), async {
        loop {
            if redis_store
                .get_session_binding("shared-session")
                .await
                .unwrap()
                .is_some_and(|binding| binding.credential_id == first_id)
            {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("background session binding should propagate between managers");

    let mut rebound = manager_b
        .acquire_context_for_session(None, Some("shared-session"), &empty)
        .await
        .unwrap();
    assert_eq!(rebound.id, first_id);
    rebound.release_in_flight();

    assert!(!manager_a.record_session_soft_failure("shared-session", first_id));
    let mut rebound_after_soft_failure = manager_b
        .acquire_context_for_session(None, Some("shared-session"), &empty)
        .await
        .unwrap();
    assert_eq!(rebound_after_soft_failure.id, first_id);
    rebound_after_soft_failure.release_in_flight();
    assert!(
        manager_b.record_session_soft_failure("shared-session", first_id),
        "同凭据重新绑定不应清空 Redis 中已有软失败计数"
    );
    manager_b.clear_session_soft_failure("shared-session", first_id);

    assert!(
        manager_a
            .report_transient_failure(first_id, None, Some(StdDuration::from_secs(30)), "429")
            .unwrap()
    );

    let mut after_cooldown = manager_b.acquire_context(None).await.unwrap();
    assert_ne!(after_cooldown.id, first_id);
    after_cooldown.release_in_flight();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unique_session_uses_one_redis_binding_read_and_background_write() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![api_key_credential("unique-session-background-binding")],
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();
    let session_id = format!("unique-session-{}", uuid::Uuid::new_v4());
    let reads_before = manager.session_binding_redis_reads.load(Ordering::Acquire);
    let writes_before = manager
        .session_binding_redis_writes_enqueued
        .load(Ordering::Acquire);

    let mut context = manager
        .acquire_context_for_session(None, Some(&session_id), &HashSet::new())
        .await
        .unwrap();

    assert_eq!(
        manager.session_binding_redis_reads.load(Ordering::Acquire) - reads_before,
        1,
        "one acquire must perform exactly one Redis session-binding lookup"
    );
    assert_eq!(
        manager
            .session_binding_redis_writes_enqueued
            .load(Ordering::Acquire)
            - writes_before,
        1,
        "new binding must be submitted to the bounded background executor"
    );
    assert_eq!(
        sticky_bound_credential_id(&manager.session_bindings, &session_id),
        Some(context.id),
        "local sticky state must be visible before background Redis persistence completes"
    );

    tokio::time::timeout(StdDuration::from_secs(5), async {
        loop {
            if redis_store
                .get_session_binding(&session_id)
                .await
                .unwrap()
                .is_some_and(|binding| binding.credential_id == context.id)
            {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("background Redis session binding should become visible to other instances");

    context.release_in_flight();
    redis_store
        .delete_session_binding(&session_id)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conditional_unbind_fences_a_queued_session_binding_write() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![
            api_key_credential("queued-binding-a"),
            api_key_credential("queued-binding-b"),
        ],
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();

    let absent_session = format!("queued-binding-absent-{}", uuid::Uuid::new_v4());
    let absent_write_lock = manager.session_binding_write_lock(&absent_session);
    let absent_write_guard = absent_write_lock.lock().await;
    manager.bind_session_to_credential(&absent_session, 1);
    assert_eq!(
        sticky_bound_credential_id(&manager.session_bindings, &absent_session),
        Some(1)
    );
    assert!(
        redis_store
            .get_session_binding(&absent_session)
            .await
            .unwrap()
            .is_none(),
        "holding the shard lock must keep the accepted background SET out of Redis"
    );

    manager.unbind_session_if_bound_to(&absent_session, 1);
    assert_eq!(
        sticky_bound_credential_id(&manager.session_bindings, &absent_session),
        None,
        "conditional unbind must clear local-first state before Redis reconciliation"
    );
    drop(absent_write_guard);
    let drain =
        crate::kiro::token_manager::drain_best_effort_storage_tasks(StdDuration::from_secs(6))
            .await;
    assert!(
        drain.drained,
        "queued session write did not drain: {drain:?}"
    );
    assert!(
        redis_store
            .get_session_binding(&absent_session)
            .await
            .unwrap()
            .is_none(),
        "the queued SET must not resurrect an absent conditionally deleted binding"
    );

    let mismatch_session = format!("queued-binding-mismatch-{}", uuid::Uuid::new_v4());
    let mismatch_write_lock = manager.session_binding_write_lock(&mismatch_session);
    let mismatch_write_guard = mismatch_write_lock.lock().await;
    manager.bind_session_to_credential(&mismatch_session, 1);
    let authoritative = SchedulerSessionBinding {
        credential_id: 2,
        last_used_at: Utc::now(),
        soft_failure_count: 0,
    };
    redis_store
        .set_session_binding(
            &mismatch_session,
            &authoritative,
            SESSION_BINDING_TTL_SECS as usize,
        )
        .await
        .unwrap();

    manager.unbind_session_if_bound_to(&mismatch_session, 1);
    assert_eq!(
        sticky_bound_credential_id(&manager.session_bindings, &mismatch_session),
        Some(2),
        "a different authoritative Redis binding must replace the removed local target"
    );
    drop(mismatch_write_guard);
    let drain =
        crate::kiro::token_manager::drain_best_effort_storage_tasks(StdDuration::from_secs(6))
            .await;
    assert!(
        drain.drained,
        "mismatched session write did not drain: {drain:?}"
    );
    assert_eq!(
        redis_store
            .get_session_binding(&mismatch_session)
            .await
            .unwrap(),
        Some(authoritative),
        "the older queued local SET must not overwrite the authoritative Redis binding"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redis_bulk_session_cleanup_is_background_and_does_not_touch_admission_breaker() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![api_key_credential("bulk-session-cleanup")],
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();
    let binding = SchedulerSessionBinding {
        credential_id: 1,
        last_used_at: Utc::now(),
        soft_failure_count: 0,
    };
    const SESSION_COUNT: usize = 10_000;
    futures::stream::iter(0..SESSION_COUNT)
        .for_each_concurrent(Some(64), |index| {
            let redis_store = redis_store.clone();
            let binding = binding.clone();
            async move {
                redis_store
                    .set_session_binding(
                        &format!("bulk-session-{index}"),
                        &binding,
                        SESSION_BINDING_TTL_SECS as usize,
                    )
                    .await
                    .unwrap();
            }
        })
        .await;
    manager
        .scheduler_redis_degraded_streak
        .store(3, Ordering::Release);

    let started_at = Instant::now();
    manager.unbind_sessions_for_credential(1);
    let return_latency = started_at.elapsed();

    assert!(
        return_latency < SCHEDULER_REDIS_HOT_OP_TIMEOUT,
        "bulk sticky cleanup must leave the request path, elapsed={return_latency:?}"
    );
    assert!(manager.scheduler_redis_degraded_until.lock().is_none());
    assert_eq!(
        manager
            .scheduler_redis_degraded_streak
            .load(Ordering::Acquire),
        3
    );

    for _ in 0..64 {
        let mut context =
            tokio::time::timeout(StdDuration::from_secs(1), manager.acquire_context(None))
                .await
                .expect("high-cardinality cleanup must not starve Redis admission")
                .expect("Redis admission must remain healthy during background cleanup");
        context.release_in_flight();
    }

    let drain =
        crate::kiro::token_manager::drain_best_effort_storage_tasks(StdDuration::from_secs(12))
            .await;
    assert!(drain.drained, "background cleanup did not drain: {drain:?}");
    assert_eq!(
        redis_store.delete_sessions_for_credential(1).await.unwrap(),
        0,
        "background cleanup left credential session bindings behind"
    );
    for index in [0, SESSION_COUNT / 2, SESSION_COUNT - 1] {
        assert!(
            redis_store
                .get_session_binding(&format!("bulk-session-{index}"))
                .await
                .unwrap()
                .is_none(),
            "background cleanup left bulk-session-{index} bound"
        );
    }
    assert!(manager.scheduler_redis_degraded_until.lock().is_none());
    assert_eq!(
        manager
            .scheduler_redis_degraded_streak
            .load(Ordering::Acquire),
        0,
        "successful admission probes should reset the old failure streak"
    );
}

#[tokio::test]
async fn test_only_available_credential_is_not_an_alternate_after_soft_failure() {
    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();

    let mut disabled1 = KiroCredentials::default();
    disabled1.disabled = true;
    let mut disabled2 = KiroCredentials::default();
    disabled2.disabled = true;
    let mut active = KiroCredentials::default();
    active.access_token = Some("active-token".to_string());
    active.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager = MultiTokenManager::new(
        config,
        vec![disabled1, disabled2, active],
        None,
        None,
        false,
    )
    .unwrap();
    let empty = HashSet::new();
    let ctx = manager
        .acquire_context_for_session(None, Some("session-only"), &empty)
        .await
        .unwrap();

    assert_eq!(ctx.id, 3);
    assert!(!manager.has_alternate_usable_credential(None, &empty, ctx.id));
    assert!(!manager.record_session_soft_failure("session-only", ctx.id));
    assert!(manager.record_session_soft_failure("session-only", ctx.id));
    assert!(!manager.has_alternate_usable_credential(None, &empty, ctx.id));
}

#[test]
fn test_cached_alternate_usable_credential_uses_current_memory_state() {
    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();
    config.credential_transient_cooldown_secs = 60;

    let manager = MultiTokenManager::new(
        config,
        vec![
            test_access_token_credential("t1", "Pro"),
            test_access_token_credential("t2", "Pro"),
        ],
        None,
        None,
        false,
    )
    .unwrap();
    let mut excluded = HashSet::new();

    assert!(manager.has_alternate_usable_credential_cached(None, &excluded, 1));

    manager
        .report_transient_failure_kind(1, None, TransientFailureKind::RateLimit, None, "429")
        .unwrap();

    assert!(
        manager.has_alternate_usable_credential_cached(None, &excluded, 1),
        "当前账号冷却后，本次 retry 应能用本机内存态发现另一个可调度账号"
    );

    excluded.insert(2);
    assert!(
        !manager.has_alternate_usable_credential_cached(None, &excluded, 1),
        "唯一备选账号已被本次请求排除时，不应误报可 fallback"
    );
}

#[test]
fn test_cached_alternate_usable_credential_is_false_for_single_active_credential() {
    let mut disabled = KiroCredentials::default();
    disabled.disabled = true;
    let manager = MultiTokenManager::new(
        Config::default(),
        vec![
            disabled,
            test_access_token_credential("active-token", "Pro"),
        ],
        None,
        None,
        false,
    )
    .unwrap();
    let empty = HashSet::new();

    assert!(!manager.has_alternate_usable_credential_cached(None, &empty, 2));
}

#[tokio::test]
async fn test_excluding_only_available_credential_reports_temporary_exclusion() {
    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();

    let mut disabled = KiroCredentials::default();
    disabled.disabled = true;
    let mut active = KiroCredentials::default();
    active.access_token = Some("active-token".to_string());
    active.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager =
        MultiTokenManager::new(config, vec![disabled, active], None, None, false).unwrap();
    let mut excluded = HashSet::new();
    excluded.insert(2);

    let err = manager
        .acquire_context_for_session(None, Some("session-excluded"), &excluded)
        .await
        .err()
        .unwrap()
        .to_string();

    assert!(
        err.contains("本次请求临时排除了所有可用账号"),
        "错误应提示临时排除，实际: {}",
        err
    );
    assert!(
        !err.contains("所有账号均已禁用"),
        "错误不应误报所有账号禁用，实际: {}",
        err
    );
}

#[tokio::test]
async fn test_bound_disabled_proxy_resource_is_not_dispatchable() {
    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();

    let mut blocked = KiroCredentials::default();
    blocked.access_token = Some("blocked-token".to_string());
    blocked.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    blocked.proxy_resource_id = Some(7);

    let mut active = KiroCredentials::default();
    active.access_token = Some("active-token".to_string());
    active.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager = MultiTokenManager::new(config, vec![blocked, active], None, None, false).unwrap();
    manager.proxy_resources.lock().insert(
        7,
        ProxyResourceRuntime {
            id: 7,
            name: "disabled-proxy".to_string(),
            proxy_url: "socks5h://127.0.0.1:1080".to_string(),
            proxy_username: None,
            proxy_password: None,
            enabled: false,
        },
    );

    let ctx = manager.acquire_context(None).await.unwrap();
    assert_eq!(ctx.id, 2);

    let snapshot = manager.snapshot();
    let blocked = snapshot.entries.iter().find(|entry| entry.id == 1).unwrap();
    assert_eq!(blocked.effective_proxy_source, "resource_disabled");
    assert_eq!(blocked.effective_proxy_url, None);
}

#[tokio::test]
async fn test_all_proxy_blocked_credentials_fail_fast_with_proxy_error() {
    let mut config = Config::default();
    config.proxy_url = Some("http://global-proxy:8080".to_string());
    config.credential_dispatch_max_wait_secs = 1;

    let mut missing_proxy = test_access_token_credential("missing-proxy", "Pro");
    missing_proxy.proxy_resource_id = Some(404);
    let mut disabled_proxy = test_access_token_credential("disabled-proxy", "Pro");
    disabled_proxy.proxy_resource_id = Some(7);

    let manager = MultiTokenManager::new(
        config,
        vec![missing_proxy, disabled_proxy],
        None,
        None,
        false,
    )
    .unwrap();
    manager.proxy_resources.lock().insert(
        7,
        ProxyResourceRuntime {
            id: 7,
            name: "disabled-proxy".to_string(),
            proxy_url: "socks5h://127.0.0.1:1080".to_string(),
            proxy_username: None,
            proxy_password: None,
            enabled: false,
        },
    );

    let started = Instant::now();
    let err = manager
        .acquire_context(None)
        .await
        .err()
        .unwrap()
        .to_string();

    assert!(
        started.elapsed() < StdDuration::from_millis(200),
        "全部凭据代理资源不可用时应快速失败，不应进入容量等待"
    );
    assert!(
        err.contains("代理资源不可用"),
        "错误应明确提示代理资源不可用，实际: {}",
        err
    );
    assert!(
        !err.contains("所有账号均已禁用") && !err.contains("没有支持当前模型"),
        "代理不可用不应误报禁用或模型不兼容，实际: {}",
        err
    );
    assert_eq!(manager.snapshot().queued_requests, 0);
}

#[tokio::test]
async fn test_bound_missing_proxy_resource_does_not_fallback_to_global_proxy() {
    let mut config = Config::default();
    config.proxy_url = Some("http://global-proxy:8080".to_string());

    let mut credential = KiroCredentials::default();
    credential.access_token = Some("token".to_string());
    credential.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    credential.proxy_resource_id = Some(404);

    let manager = MultiTokenManager::new(config, vec![credential], None, None, false).unwrap();
    let err = manager
        .acquire_context_for_credential(1)
        .await
        .err()
        .unwrap()
        .to_string();

    assert!(
        err.contains("代理资源 #404 不存在"),
        "应返回代理资源缺失错误，实际: {}",
        err
    );
    assert!(
        err.contains("阻止回退"),
        "不应静默回退到全局代理，实际: {}",
        err
    );

    let snapshot = manager.snapshot();
    let entry = snapshot.entries.iter().find(|entry| entry.id == 1).unwrap();
    assert_eq!(entry.effective_proxy_source, "resource_missing");
    assert_eq!(entry.effective_proxy_url, None);
}

#[test]
fn test_external_import_refresh_preserves_bound_proxy_resource() {
    let mut config = Config::default();
    config.proxy_url = Some("http://global-proxy:8080".to_string());

    let manager = MultiTokenManager::new(config, vec![], None, None, false).unwrap();
    manager.proxy_resources.lock().insert(
        7,
        ProxyResourceRuntime {
            id: 7,
            name: "import-proxy".to_string(),
            proxy_url: "socks5h://127.0.0.1:1080".to_string(),
            proxy_username: Some("user".to_string()),
            proxy_password: Some("pass".to_string()),
            enabled: true,
        },
    );

    let mut source = KiroCredentials::default();
    source.proxy_resource_id = Some(7);
    let mut refreshed = KiroCredentials::default();
    refreshed.access_token = Some("refreshed-token".to_string());
    refreshed.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let preserved = MultiTokenManager::preserve_proxy_fields(refreshed, &source);
    assert_eq!(preserved.proxy_resource_id, Some(7));

    let resolved = manager.resolve_proxy_for_credential(preserved).unwrap();
    assert_eq!(
        resolved.proxy_url.as_deref(),
        Some("socks5h://127.0.0.1:1080")
    );
    assert_eq!(resolved.proxy_username.as_deref(), Some("user"));
    assert_eq!(resolved.proxy_password.as_deref(), Some("pass"));
    assert_eq!(
        resolved
            .effective_proxy(manager.proxy.as_ref())
            .unwrap()
            .url,
        "socks5h://127.0.0.1:1080"
    );
}

#[tokio::test]
async fn test_unbind_session_if_bound_to_does_not_clear_original_binding() {
    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();

    let mut cred1 = KiroCredentials::default();
    cred1.access_token = Some("t1".to_string());
    cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    let mut cred2 = KiroCredentials::default();
    cred2.access_token = Some("t2".to_string());
    cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager = MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();
    let empty = HashSet::new();

    let bound = manager
        .acquire_context_for_session(None, Some("session-c"), &empty)
        .await
        .unwrap();

    let mut excluded = HashSet::new();
    excluded.insert(bound.id);
    let fallback = manager
        .acquire_context_for_session(None, Some("session-c"), &excluded)
        .await
        .unwrap();
    assert_ne!(bound.id, fallback.id);

    manager.unbind_session_if_bound_to("session-c", fallback.id);

    let rebound = manager
        .acquire_context_for_session(None, Some("session-c"), &empty)
        .await
        .unwrap();
    assert_eq!(bound.id, rebound.id);
}

#[test]
fn pending_local_session_binding_rejects_stale_redis_reconciliation() {
    let bindings = Mutex::new(HashMap::new());
    let first = bind_sticky_session_to_credential(&bindings, "pending-session", 1, true);

    cache_sticky_redis_binding(&bindings, "pending-session", None);
    assert_eq!(
        sticky_bound_credential_id(&bindings, "pending-session"),
        Some(1),
        "a Redis miss must not erase a local-first binding before its queued write finishes"
    );

    let second = bind_sticky_session_to_credential(&bindings, "pending-session", 2, true);
    assert!(!cache_sticky_redis_binding_if_current(
        &bindings,
        "pending-session",
        &first,
        Some(SchedulerSessionBinding {
            credential_id: 1,
            last_used_at: first.last_used_at,
            soft_failure_count: 0,
        }),
    ));
    assert_eq!(
        sticky_bound_credential_id(&bindings, "pending-session"),
        Some(2),
        "an older background completion must not overwrite a newer local binding"
    );

    assert!(clear_sticky_redis_persist_pending_if_current(
        &bindings,
        "pending-session",
        &second,
    ));
    cache_sticky_redis_binding(&bindings, "pending-session", None);
    assert_eq!(
        sticky_bound_credential_id(&bindings, "pending-session"),
        None
    );
}

#[test]
fn unpolled_session_binding_write_clears_pending_state_on_cancellation() {
    let bindings = Arc::new(Mutex::new(HashMap::new()));
    let expected = bind_sticky_session_to_credential(&bindings, "cancelled-session", 1, true);
    let guard = Arc::new(SessionBindingPersistGuard {
        session_bindings: bindings.clone(),
        session_id: "cancelled-session".to_string(),
        expected: expected.clone(),
        armed: AtomicBool::new(true),
    });
    let future = {
        let guard = guard.clone();
        async move {
            std::future::pending::<()>().await;
            drop(guard);
        }
    };
    drop(guard);
    assert!(sticky_redis_binding_matches_local(
        &bindings,
        "cancelled-session",
        &expected,
    ));

    drop(future);
    assert!(
        !sticky_redis_binding_matches_local(&bindings, "cancelled-session", &expected),
        "dropping an accepted-but-unpolled write future must clear its pending marker"
    );
    assert_eq!(
        sticky_bound_credential_id(&bindings, "cancelled-session"),
        Some(1),
        "cancelling Redis persistence must not discard the local-first binding"
    );
}

#[tokio::test]
async fn test_current_id_respects_opus_model_filter() {
    let mut free = KiroCredentials::default();
    free.priority = 0;
    free.subscription_title = Some("Free".to_string());
    free.access_token = Some("free-token".to_string());
    free.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let mut pro = KiroCredentials::default();
    pro.priority = 1;
    pro.subscription_title = Some("Pro".to_string());
    pro.access_token = Some("pro-token".to_string());
    pro.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager =
        MultiTokenManager::new(Config::default(), vec![free, pro], None, None, false).unwrap();

    let ctx = manager
        .acquire_context(Some("claude-opus-4"))
        .await
        .unwrap();
    assert_eq!(ctx.id, 2);
    assert_eq!(ctx.token, "pro-token");
}

#[tokio::test]
async fn test_sonnet_model_can_use_free_credentials() {
    let mut free = KiroCredentials::default();
    free.priority = 0;
    free.subscription_title = Some("Free".to_string());
    free.access_token = Some("free-token".to_string());
    free.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let mut pro = KiroCredentials::default();
    pro.priority = 1;
    pro.subscription_title = Some("Pro".to_string());
    pro.access_token = Some("pro-token".to_string());
    pro.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager =
        MultiTokenManager::new(Config::default(), vec![free, pro], None, None, false).unwrap();

    let mut ctx = manager.acquire_context(Some(SONNET_MODEL)).await.unwrap();
    assert_eq!(ctx.id, 1);
    assert_eq!(ctx.token, "free-token");
    ctx.release_in_flight();
}

#[tokio::test]
async fn test_sonnet_bound_session_falls_back_when_bound_credential_is_full() {
    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();
    config.credential_max_concurrent_requests = 1;

    let mut cred1 = KiroCredentials::default();
    cred1.subscription_title = Some("Free".to_string());
    cred1.access_token = Some("t1".to_string());
    cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    let mut cred2 = KiroCredentials::default();
    cred2.subscription_title = Some("Free".to_string());
    cred2.access_token = Some("t2".to_string());
    cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager = MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();
    let empty = HashSet::new();

    let mut bound = manager
        .acquire_context_for_session(Some(SONNET_MODEL), Some("sonnet-sticky-full"), &empty)
        .await
        .unwrap();
    manager.report_success_for_session(bound.id, Some("sonnet-sticky-full"));

    let mut fallback = manager
        .acquire_context_for_session(Some(SONNET_MODEL), Some("sonnet-sticky-full"), &empty)
        .await
        .unwrap();

    assert_ne!(bound.id, fallback.id);
    assert!(fallback.fallback_from_sticky);
    assert!(!fallback.sticky_bound);

    fallback.release_in_flight();
    bound.release_in_flight();

    let mut rebound = manager
        .acquire_context_for_session(Some(SONNET_MODEL), Some("sonnet-sticky-full"), &empty)
        .await
        .unwrap();
    assert_eq!(rebound.id, bound.id);
    rebound.release_in_flight();
}

#[tokio::test]
async fn test_sonnet_rate_limiter_prefers_other_dispatchable_credential() {
    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();
    config.credential_rpm = Some(1);

    let mut cred1 = KiroCredentials::default();
    cred1.subscription_title = Some("Free".to_string());
    cred1.access_token = Some("t1".to_string());
    cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    let mut cred2 = KiroCredentials::default();
    cred2.subscription_title = Some("Free".to_string());
    cred2.access_token = Some("t2".to_string());
    cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager = MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

    let mut first = manager.acquire_context(Some(SONNET_MODEL)).await.unwrap();
    let mut second = manager.acquire_context(Some(SONNET_MODEL)).await.unwrap();

    assert_ne!(first.id, second.id);
    first.release_in_flight();
    second.release_in_flight();
}

#[tokio::test]
async fn test_sonnet_rate_limit_cooldown_skips_limited_credential() {
    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();
    config.credential_rate_limit_cooldown_secs = 60;
    config.credential_max_cooldown_secs = 60;
    config.credential_cooldown_jitter_percent = 0;

    let mut cred1 = KiroCredentials::default();
    cred1.subscription_title = Some("Free".to_string());
    cred1.access_token = Some("t1".to_string());
    cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
    let mut cred2 = KiroCredentials::default();
    cred2.subscription_title = Some("Free".to_string());
    cred2.access_token = Some("t2".to_string());
    cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

    let manager = MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

    assert!(
        manager
            .report_transient_failure_kind(
                1,
                Some(SONNET_MODEL),
                TransientFailureKind::RateLimit,
                None,
                "429 Too Many Requests"
            )
            .unwrap()
    );

    let mut ctx = manager.acquire_context(Some(SONNET_MODEL)).await.unwrap();
    assert_eq!(ctx.id, 2);
    ctx.release_in_flight();
}

#[tokio::test]
async fn test_sonnet_pool_uses_available_credentials_when_one_is_cooldown_and_sticky_is_full() {
    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();
    config.credential_max_concurrent_requests = 1;
    config.credential_auth_error_cooldown_secs = 60;
    config.credential_max_cooldown_secs = 60;
    config.credential_cooldown_jitter_percent = 0;

    let credentials = (1..=4)
        .map(|idx| {
            let mut cred = KiroCredentials::default();
            cred.subscription_title = Some("Free".to_string());
            cred.access_token = Some(format!("t{idx}"));
            cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
            cred
        })
        .collect();

    let manager = MultiTokenManager::new(config, credentials, None, None, false).unwrap();
    let empty = HashSet::new();

    assert!(
        manager
            .report_transient_failure_kind(
                1,
                Some(SONNET_MODEL),
                TransientFailureKind::Auth,
                None,
                "403 Forbidden user is not authorized"
            )
            .unwrap()
    );

    let mut bound = manager
        .acquire_context_for_session(Some(SONNET_MODEL), Some("sonnet-pool-session"), &empty)
        .await
        .unwrap();
    assert_ne!(bound.id, 1);
    manager.report_success_for_session(bound.id, Some("sonnet-pool-session"));

    let mut fallback = manager
        .acquire_context_for_session(Some(SONNET_MODEL), Some("sonnet-pool-session"), &empty)
        .await
        .unwrap();
    assert_ne!(fallback.id, 1);
    assert_ne!(fallback.id, bound.id);
    assert!(fallback.fallback_from_sticky);
    assert!(!fallback.sticky_bound);

    let mut unbound = manager.acquire_context(Some(SONNET_MODEL)).await.unwrap();
    assert_ne!(unbound.id, 1);
    assert_ne!(unbound.id, bound.id);
    assert_ne!(unbound.id, fallback.id);

    unbound.release_in_flight();
    fallback.release_in_flight();
    bound.release_in_flight();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn test_sonnet_high_concurrency_dispatch_respects_limits_and_spreads_load() {
    const CREDENTIAL_COUNT: usize = 24;
    const COOLED_DOWN_CREDENTIALS: u64 = 4;
    const AVAILABLE_CREDENTIALS: usize = CREDENTIAL_COUNT - COOLED_DOWN_CREDENTIALS as usize;
    const REQUEST_COUNT: usize = 600;
    const PER_CREDENTIAL_LIMIT: u32 = 3;
    const GLOBAL_LIMIT: u32 = 48;

    let mut config = Config::default();
    config.load_balancing_mode = "balanced".to_string();
    config.credential_max_concurrent_requests = PER_CREDENTIAL_LIMIT;
    config.dispatch_global_max_concurrent_requests = GLOBAL_LIMIT;
    config.dispatch_max_queued_requests = REQUEST_COUNT as u32;
    config.credential_dispatch_max_wait_secs = 5;
    config.credential_rate_limit_cooldown_secs = 30;
    config.credential_auth_error_cooldown_secs = 30;
    config.credential_max_cooldown_secs = 30;
    config.credential_cooldown_jitter_percent = 0;

    let credentials = (1..=CREDENTIAL_COUNT)
        .map(|idx| {
            let mut cred = KiroCredentials::default();
            cred.subscription_title = Some("Free".to_string());
            cred.email = Some(format!("sonnet-free-{idx}@example.test"));
            cred.access_token = Some(format!("t{idx}"));
            cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
            cred
        })
        .collect();
    let manager = Arc::new(MultiTokenManager::new(config, credentials, None, None, false).unwrap());

    for id in 1..=COOLED_DOWN_CREDENTIALS {
        let kind = if id % 2 == 0 {
            TransientFailureKind::RateLimit
        } else {
            TransientFailureKind::Auth
        };
        assert!(
            manager
                .report_transient_failure_kind(
                    id,
                    Some(SONNET_MODEL),
                    kind,
                    None,
                    "preload high-concurrency cooldown"
                )
                .unwrap()
        );
    }

    let start = Arc::new(tokio::sync::Barrier::new(REQUEST_COUNT + 1));
    let mut handles = Vec::with_capacity(REQUEST_COUNT);

    for idx in 0..REQUEST_COUNT {
        let manager = manager.clone();
        let start = start.clone();
        handles.push(tokio::spawn(async move {
            start.wait().await;
            let mut ctx = manager.acquire_context(Some(SONNET_MODEL)).await.unwrap();
            assert!(
                ctx.id > COOLED_DOWN_CREDENTIALS,
                "冷却凭据不应被调度，实际选中 #{}",
                ctx.id
            );

            let snapshot = manager.snapshot();
            assert!(
                snapshot.global_in_flight_requests <= GLOBAL_LIMIT,
                "全局并发超限: {} > {}",
                snapshot.global_in_flight_requests,
                GLOBAL_LIMIT
            );
            assert!(
                snapshot.queued_requests <= REQUEST_COUNT as u32,
                "等待队列超出测试配置: {}",
                snapshot.queued_requests
            );
            for entry in &snapshot.entries {
                if entry.id <= COOLED_DOWN_CREDENTIALS {
                    assert_eq!(
                        entry.in_flight_requests, 0,
                        "冷却凭据 #{} 不应持有 in-flight",
                        entry.id
                    );
                }
                if entry.max_concurrent_requests > 0 {
                    assert!(
                        entry.in_flight_requests <= entry.max_concurrent_requests,
                        "凭据 #{} 并发超限: {} > {}",
                        entry.id,
                        entry.in_flight_requests,
                        entry.max_concurrent_requests
                    );
                }
            }

            tokio::time::sleep(StdDuration::from_millis(3 + (idx % 7) as u64)).await;
            manager.report_success(ctx.id);
            let id = ctx.id;
            ctx.release_in_flight();
            id
        }));
    }

    let started_at = Instant::now();
    start.wait().await;

    let mut selection_counts: HashMap<u64, usize> = HashMap::new();
    for handle in handles {
        let selected_id = tokio::time::timeout(StdDuration::from_secs(10), handle)
            .await
            .expect("高并发调度任务不应超时")
            .expect("高并发调度任务不应 panic");
        *selection_counts.entry(selected_id).or_insert(0) += 1;
    }

    let elapsed = started_at.elapsed();
    let snapshot = manager.snapshot();
    assert_eq!(snapshot.global_in_flight_requests, 0);
    assert_eq!(snapshot.queued_requests, 0);
    assert_eq!(
        selection_counts.len(),
        AVAILABLE_CREDENTIALS,
        "所有非冷却凭据都应在高并发下被使用，实际分布: {:?}",
        selection_counts
    );
    assert!(
        selection_counts
            .keys()
            .all(|id| *id > COOLED_DOWN_CREDENTIALS),
        "冷却凭据不应出现在最终调度分布中: {:?}",
        selection_counts
    );

    let min_selected = selection_counts.values().copied().min().unwrap_or(0);
    let max_selected = selection_counts.values().copied().max().unwrap_or(0);
    let mut distribution: Vec<_> = selection_counts
        .iter()
        .map(|(id, count)| (*id, *count))
        .collect();
    distribution.sort_by_key(|(id, _)| *id);
    println!(
        "sonnet high concurrency dispatch: requests={}, total_credentials={}, cooled_down={}, used_credentials={}, global_limit={}, per_credential_limit={}, elapsed_ms={}, min_selected={}, max_selected={}, distribution={:?}",
        REQUEST_COUNT,
        CREDENTIAL_COUNT,
        COOLED_DOWN_CREDENTIALS,
        selection_counts.len(),
        GLOBAL_LIMIT,
        PER_CREDENTIAL_LIMIT,
        elapsed.as_millis(),
        min_selected,
        max_selected,
        distribution
    );
    assert!(
        max_selected <= min_selected * 2 + 10,
        "balanced 高并发分布过度倾斜: min={}, max={}, elapsed={:?}, counts={:?}",
        min_selected,
        max_selected,
        elapsed,
        selection_counts
    );
}

#[test]
fn test_multi_token_manager_report_refresh_failure() {
    let config = Config::default();
    let cred1 = KiroCredentials::default();
    let cred2 = KiroCredentials::default();

    let manager = MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

    assert_eq!(manager.available_count(), 2);
    for _ in 0..(MAX_FAILURES_PER_CREDENTIAL - 1) {
        assert!(manager.report_refresh_failure(1));
    }
    assert_eq!(manager.available_count(), 2);

    assert!(manager.report_refresh_failure(1));
    assert_eq!(manager.available_count(), 1);

    let snapshot = manager.snapshot();
    let first = snapshot.entries.iter().find(|e| e.id == 1).unwrap();
    assert!(first.disabled);
    assert_eq!(first.refresh_failure_count, MAX_FAILURES_PER_CREDENTIAL);
    assert_eq!(snapshot.current_id, 2);
}

#[tokio::test]
async fn test_multi_token_manager_refresh_failure_disabled_is_not_auto_recovered() {
    let config = Config::default();
    let cred1 = KiroCredentials::default();
    let cred2 = KiroCredentials::default();

    let manager = MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

    for _ in 0..MAX_FAILURES_PER_CREDENTIAL {
        manager.report_refresh_failure(1);
        manager.report_refresh_failure(2);
    }
    assert_eq!(manager.available_count(), 0);

    let err = manager
        .acquire_context(None)
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(
        err.contains("所有账号均已禁用"),
        "错误应提示所有账号禁用，实际: {}",
        err
    );
}

#[test]
fn test_multi_token_manager_report_quota_exhausted() {
    let config = Config::default();
    let cred1 = KiroCredentials::default();
    let cred2 = KiroCredentials::default();

    let manager = MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

    // 凭据会自动分配 ID（从 1 开始）
    assert_eq!(manager.available_count(), 2);
    assert!(manager.report_quota_exhausted(1));
    assert_eq!(manager.available_count(), 1);

    // 再禁用第二个后，无可用凭据
    assert!(!manager.report_quota_exhausted(2));
    assert_eq!(manager.available_count(), 0);
}

#[test]
fn test_report_risk_controlled_disables_with_specific_reason() {
    let config = Config::default();
    let cred1 = KiroCredentials::default();
    let cred2 = KiroCredentials::default();

    let manager = MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

    assert!(manager.report_risk_controlled(
        1,
        CredentialRiskControlReason::TemporarilySuspended,
        "TEMPORARILY_SUSPENDED"
    ));

    let snapshot = manager.snapshot();
    assert_eq!(snapshot.available, 1);
    assert_eq!(snapshot.current_id, 2);
    let disabled = snapshot.entries.iter().find(|entry| entry.id == 1).unwrap();
    assert!(disabled.disabled);
    assert_eq!(
        disabled.disabled_reason.as_deref(),
        Some("TemporarilySuspended")
    );
    assert_eq!(disabled.failure_count, MAX_FAILURES_PER_CREDENTIAL);
}

#[tokio::test]
async fn test_multi_token_manager_quota_disabled_is_not_auto_recovered() {
    let config = Config::default();
    let cred1 = KiroCredentials::default();
    let cred2 = KiroCredentials::default();

    let manager = MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

    manager.report_quota_exhausted(1);
    manager.report_quota_exhausted(2);
    assert_eq!(manager.available_count(), 0);

    let err = manager
        .acquire_context(None)
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(
        err.contains("所有账号均已禁用"),
        "错误应提示所有账号禁用，实际: {}",
        err
    );
    assert_eq!(manager.available_count(), 0);
}

// ============ 凭据级 Region 优先级测试 ============

#[test]
fn test_credential_region_priority_uses_credential_auth_region() {
    // 凭据配置了 auth_region 时，应使用凭据的 auth_region
    let mut config = Config::default();
    config.region = "us-west-2".to_string();

    let mut credentials = KiroCredentials::default();
    credentials.auth_region = Some("eu-west-1".to_string());

    let region = credentials.effective_auth_region(&config);
    assert_eq!(region, "eu-west-1");
}

#[test]
fn test_credential_region_priority_fallback_to_credential_region() {
    // 凭据未配置 auth_region 但配置了 region 时，应回退到凭据.region
    let mut config = Config::default();
    config.region = "us-west-2".to_string();

    let mut credentials = KiroCredentials::default();
    credentials.region = Some("eu-central-1".to_string());

    let region = credentials.effective_auth_region(&config);
    assert_eq!(region, "eu-central-1");
}

#[test]
fn test_credential_region_priority_fallback_to_config() {
    // 凭据未配置 auth_region 和 region 时，应回退到 config
    let mut config = Config::default();
    config.region = "us-west-2".to_string();

    let credentials = KiroCredentials::default();
    assert!(credentials.auth_region.is_none());
    assert!(credentials.region.is_none());

    let region = credentials.effective_auth_region(&config);
    assert_eq!(region, "us-west-2");
}

#[test]
fn test_multiple_credentials_use_respective_regions() {
    // 多凭据场景下，不同凭据使用各自的 auth_region
    let mut config = Config::default();
    config.region = "ap-northeast-1".to_string();

    let mut cred1 = KiroCredentials::default();
    cred1.auth_region = Some("us-east-1".to_string());

    let mut cred2 = KiroCredentials::default();
    cred2.region = Some("eu-west-1".to_string());

    let cred3 = KiroCredentials::default(); // 无 region，使用 config

    assert_eq!(cred1.effective_auth_region(&config), "us-east-1");
    assert_eq!(cred2.effective_auth_region(&config), "eu-west-1");
    assert_eq!(cred3.effective_auth_region(&config), "ap-northeast-1");
}

#[test]
fn test_idc_oidc_endpoint_uses_credential_auth_region() {
    // 验证 IdC OIDC endpoint URL 使用凭据 auth_region
    let mut config = Config::default();
    config.region = "us-west-2".to_string();

    let mut credentials = KiroCredentials::default();
    credentials.auth_region = Some("eu-central-1".to_string());

    let region = credentials.effective_auth_region(&config);
    let refresh_url = format!("https://oidc.{}.amazonaws.com/token", region);

    assert_eq!(refresh_url, "https://oidc.eu-central-1.amazonaws.com/token");
}

#[test]
fn test_social_refresh_endpoint_uses_credential_auth_region() {
    // 验证 Social refresh endpoint URL 使用凭据 auth_region
    let mut config = Config::default();
    config.region = "us-west-2".to_string();

    let mut credentials = KiroCredentials::default();
    credentials.auth_region = Some("ap-southeast-1".to_string());

    let region = credentials.effective_auth_region(&config);
    let refresh_url = format!("https://prod.{}.auth.desktop.kiro.dev/refreshToken", region);

    assert_eq!(
        refresh_url,
        "https://prod.ap-southeast-1.auth.desktop.kiro.dev/refreshToken"
    );
}

#[test]
fn test_api_call_uses_effective_api_region() {
    // 验证 API 调用使用 effective_api_region
    let mut config = Config::default();
    config.region = "us-west-2".to_string();

    let mut credentials = KiroCredentials::default();
    credentials.region = Some("eu-west-1".to_string());

    // 凭据.region 不参与 api_region 回退链
    let api_region = credentials.effective_api_region(&config);
    let api_host = format!("q.{}.amazonaws.com", api_region);

    assert_eq!(api_host, "q.us-west-2.amazonaws.com");
}

#[test]
fn test_api_call_uses_credential_api_region() {
    // 凭据配置了 api_region 时，API 调用应使用凭据的 api_region
    let mut config = Config::default();
    config.region = "us-west-2".to_string();

    let mut credentials = KiroCredentials::default();
    credentials.api_region = Some("eu-central-1".to_string());

    let api_region = credentials.effective_api_region(&config);
    let api_host = format!("q.{}.amazonaws.com", api_region);

    assert_eq!(api_host, "q.eu-central-1.amazonaws.com");
}

#[test]
fn test_region_update_preserves_matching_profile_arn() {
    let mut credential = KiroCredentials {
        auth_method: Some("idc".to_string()),
        access_token: Some("access".to_string()),
        expires_at: Some("2099-01-01T00:00:00Z".to_string()),
        profile_arn: Some(
            "arn:aws:codewhisperer:eu-central-1:123456789012:profile/REAL".to_string(),
        ),
        ..Default::default()
    };
    credential.id = Some(1);
    let manager =
        MultiTokenManager::new(Config::default(), vec![credential], None, None, false).unwrap();

    manager
        .set_credential_regions(1, None, None, Some(Some("eu-central-1".to_string())))
        .unwrap();

    let entry = manager
        .snapshot()
        .entries
        .into_iter()
        .find(|entry| entry.id == 1)
        .unwrap();
    assert_eq!(entry.api_region.as_deref(), Some("eu-central-1"));
    assert_eq!(entry.effective_api_region, "eu-central-1");
    assert!(entry.has_profile_arn);
    assert!(entry.expires_at.is_none());
}

#[test]
fn test_region_update_clears_conflicting_profile_arn() {
    let mut credential = KiroCredentials {
        auth_method: Some("idc".to_string()),
        profile_arn: Some(
            "arn:aws:codewhisperer:eu-central-1:123456789012:profile/REAL".to_string(),
        ),
        ..Default::default()
    };
    credential.id = Some(1);
    let manager =
        MultiTokenManager::new(Config::default(), vec![credential], None, None, false).unwrap();

    manager
        .set_credential_regions(1, None, None, Some(Some("us-east-1".to_string())))
        .unwrap();

    let entry = manager
        .snapshot()
        .entries
        .into_iter()
        .find(|entry| entry.id == 1)
        .unwrap();
    assert_eq!(entry.api_region.as_deref(), Some("us-east-1"));
    assert_eq!(entry.effective_api_region, "us-east-1");
    assert!(!entry.has_profile_arn);
}

#[test]
fn test_credential_region_empty_string_treated_as_set() {
    // 空字符串 auth_region 被视为已设置（虽然不推荐，但行为应一致）
    let mut config = Config::default();
    config.region = "us-west-2".to_string();

    let mut credentials = KiroCredentials::default();
    credentials.auth_region = Some("".to_string());

    let region = credentials.effective_auth_region(&config);
    // 空字符串被视为已设置，不会回退到 config
    assert_eq!(region, "");
}

#[test]
fn test_auth_and_api_region_independent() {
    // auth_region 和 api_region 互不影响
    let mut config = Config::default();
    config.region = "default".to_string();

    let mut credentials = KiroCredentials::default();
    credentials.auth_region = Some("auth-only".to_string());
    credentials.api_region = Some("api-only".to_string());

    assert_eq!(credentials.effective_auth_region(&config), "auth-only");
    assert_eq!(credentials.effective_api_region(&config), "api-only");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn postgres_add_credential_is_atomic_with_initial_runtime_state() {
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };
    sqlx::query(
        r#"
        ALTER TABLE credential_runtime_state
        ADD CONSTRAINT test_manager_atomic_add_runtime_failure
        CHECK (warmup_remaining <> 99)
        "#,
    )
    .execute(store.pool())
    .await
    .unwrap();
    let mut config = Config::default();
    config.credential_warmup_requests = 99;
    let manager = MultiTokenManager::new_with_stores(
        config,
        Vec::new(),
        None,
        None,
        false,
        Some(store.clone()),
        None,
    )
    .unwrap();
    let credential = api_key_credential("manager-atomic-add");

    assert!(manager.add_credential(credential.clone()).await.is_err());
    assert_eq!(manager.total_count(), 0);
    let credential_count: i64 = sqlx::query_scalar("SELECT COUNT(*)::bigint FROM credentials")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(credential_count, 0);
    let runtime_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*)::bigint FROM credential_runtime_state")
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!(runtime_count, 0);

    sqlx::query(
        "ALTER TABLE credential_runtime_state DROP CONSTRAINT test_manager_atomic_add_runtime_failure",
    )
    .execute(store.pool())
    .await
    .unwrap();
    let id = manager.add_credential(credential).await.unwrap();
    let persisted = store
        .load_credentials()
        .await
        .unwrap()
        .into_iter()
        .find(|credential| credential.id == Some(id))
        .unwrap();
    assert_eq!(persisted.storage_revision, 1);
    let runtime = store
        .load_credential_runtime_state()
        .await
        .unwrap()
        .remove(&id)
        .unwrap();
    assert_eq!(runtime.warmup_remaining, 99);
    assert_eq!(runtime.revision, 1);
    {
        let entries = manager.entries.lock();
        let local = entries.iter().find(|entry| entry.id == id).unwrap();
        assert_eq!(local.credentials.storage_revision, 1);
        assert_eq!(local.runtime_revision, 1);
        assert_eq!(local.warmup_remaining, 99);
    }

    store.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn postgres_auth_reset_updates_credential_and_runtime_atomically() {
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };
    let (credential, runtime) = store
        .insert_credential_with_runtime_patch(
            &KiroCredentials {
                kiro_api_key: Some("manager-auth-reset-old".to_string()),
                auth_method: Some("api_key".to_string()),
                disabled: true,
                ..Default::default()
            },
            uuid::Uuid::new_v4(),
            &CredentialRuntimeStatePatch {
                failure_count: Some(8),
                refresh_failure_count: Some(9),
                disabled_reason: CredentialRuntimeDisabledReasonPatch::Set(
                    DisabledReason::Manual.as_str().to_string(),
                ),
                credential_disabled: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let id = credential.id.unwrap();
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![credential.clone()],
        None,
        None,
        false,
        Some(store.clone()),
        None,
    )
    .unwrap();
    let baseline = store
        .load_credentials()
        .await
        .unwrap()
        .into_iter()
        .find(|credential| credential.id == Some(id))
        .unwrap();
    assert_eq!(baseline.storage_revision, 2);
    {
        let entries = manager.entries.lock();
        let local = entries.iter().find(|entry| entry.id == id).unwrap();
        assert_eq!(
            local.credentials.storage_revision,
            baseline.storage_revision
        );
        assert_eq!(
            local.credentials.machine_id.as_deref(),
            baseline.machine_id.as_deref()
        );
    }
    {
        let mut entries = manager.entries.lock();
        let entry = entries.iter_mut().find(|entry| entry.id == id).unwrap();
        entry.failure_count = runtime.state.failure_count;
        entry.refresh_failure_count = runtime.state.refresh_failure_count;
        entry.runtime_revision = runtime.state.revision;
        entry.disabled = true;
        entry.disabled_reason = Some(DisabledReason::Manual);
    }
    sqlx::query(
        r#"
        ALTER TABLE credential_runtime_state
        ADD CONSTRAINT test_manager_atomic_auth_runtime_failure
        CHECK (failure_count <> 0)
        "#,
    )
    .execute(store.pool())
    .await
    .unwrap();
    let update = CredentialAuthUpdate {
        kiro_api_key: Some("manager-auth-reset-new".to_string()),
        auth_method: Some("api_key".to_string()),
        ..Default::default()
    };

    assert!(
        manager
            .update_credential_auth(id, update.clone(), true)
            .is_err()
    );
    let after_failure = store
        .load_credentials()
        .await
        .unwrap()
        .into_iter()
        .find(|credential| credential.id == Some(id))
        .unwrap();
    assert_eq!(after_failure.storage_revision, baseline.storage_revision);
    assert_eq!(
        after_failure.kiro_api_key.as_deref(),
        Some("manager-auth-reset-old")
    );
    assert!(after_failure.disabled);
    let after_failure_runtime = store
        .load_credential_runtime_state()
        .await
        .unwrap()
        .remove(&id)
        .unwrap();
    assert_eq!(after_failure_runtime.failure_count, 8);
    assert_eq!(after_failure_runtime.refresh_failure_count, 9);
    assert_eq!(after_failure_runtime.generation, 0);
    assert_eq!(after_failure_runtime.revision, 1);

    sqlx::query(
        "ALTER TABLE credential_runtime_state DROP CONSTRAINT test_manager_atomic_auth_runtime_failure",
    )
    .execute(store.pool())
    .await
    .unwrap();
    manager.update_credential_auth(id, update, true).unwrap();
    let persisted = store
        .load_credentials()
        .await
        .unwrap()
        .into_iter()
        .find(|credential| credential.id == Some(id))
        .unwrap();
    assert_eq!(
        persisted.storage_revision,
        baseline.storage_revision.saturating_add(1)
    );
    assert_eq!(
        persisted.kiro_api_key.as_deref(),
        Some("manager-auth-reset-new")
    );
    assert!(!persisted.disabled);
    let runtime = store
        .load_credential_runtime_state()
        .await
        .unwrap()
        .remove(&id)
        .unwrap();
    assert_eq!(runtime.failure_count, 0);
    assert_eq!(runtime.refresh_failure_count, 0);
    assert_eq!(runtime.disabled_reason, None);
    assert_eq!(runtime.generation, 1);
    assert_eq!(runtime.revision, 2);
    {
        let entries = manager.entries.lock();
        let local = entries.iter().find(|entry| entry.id == id).unwrap();
        assert_eq!(
            local.credentials.storage_revision,
            baseline.storage_revision.saturating_add(1)
        );
        assert_eq!(
            local.credentials.kiro_api_key.as_deref(),
            Some("manager-auth-reset-new")
        );
        assert_eq!(local.failure_count, 0);
        assert_eq!(local.refresh_failure_count, 0);
        assert_eq!(local.runtime_generation, 1);
        assert_eq!(local.runtime_revision, 2);
        assert!(!local.disabled);
        assert_eq!(local.disabled_reason, None);
    }

    store.drop_test_schema().await.unwrap();
}
