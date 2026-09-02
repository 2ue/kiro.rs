use super::*;
use crate::anthropic::usage::sampled_request_rejection_usage_record;
use crate::kiro::token_manager::refresh::{is_token_expiring_soon, refresh_token};
use crate::storage::postgres::CredentialAccountInfoRow;
use std::sync::Arc;

const SONNET_MODEL: &str = "claude-sonnet-4.5";

#[test]
fn automatic_recovery_revision_fence_accepts_metadata_only_advance_and_rejects_regression() {
    for round in 1..=5 {
        let rejected = format!("rejected-access-{round}");
        let mut credentials = KiroCredentials {
            access_token: Some(rejected.clone()),
            storage_revision: 8,
            ..Default::default()
        };
        assert!(
            MultiTokenManager::automatic_recovery_context_is_current(&credentials, &rejected, 7)
                .expect("higher metadata revision with the same rejected token remains current")
        );
        credentials.access_token = Some(format!("replacement-{round}"));
        assert!(
            !MultiTokenManager::automatic_recovery_context_is_current(&credentials, &rejected, 7)
                .expect("a replaced access token is no longer current")
        );
        credentials.storage_revision = 6;
        assert!(
            MultiTokenManager::automatic_recovery_context_is_current(&credentials, &rejected, 7)
                .is_err(),
            "round {round}: revision regression must fail closed"
        );
    }
}

#[test]
fn refresh_reset_generation_invalidates_old_leader_without_replacing_its_arc() {
    for round in 1..=5 {
        let state = Arc::new(CredentialRefreshState::default());
        let captured = state.generation();
        assert!(state.is_generation_current(captured), "round {round}");
        state.invalidate();
        assert!(!state.is_generation_current(captured), "round {round}");
        assert_eq!(state.generation(), captured + 1, "round {round}");
    }
}

#[test]
fn token_refresh_admission_config_is_fail_fast_at_startup_and_runtime_update() {
    for round in 1..=5 {
        let mut invalid = Config::default();
        invalid.token_refresh_max_rpm = 0;
        assert!(
            MultiTokenManager::new(invalid, Vec::new(), None, None, false).is_err(),
            "round {round}: file/startup config must not be silently clamped"
        );

        let manager = MultiTokenManager::new(Config::default(), Vec::new(), None, None, false)
            .expect("valid token refresh admission config");
        let original = manager.runtime_config().token_refresh_burst;
        assert!(
            manager
                .update_runtime_config(|config| config.token_refresh_burst = 0)
                .is_err(),
            "round {round}: runtime config must reject an out-of-range burst"
        );
        assert_eq!(manager.runtime_config().token_refresh_burst, original);
    }
}

#[test]
fn refresh_negative_result_is_typed_versioned_bounded_and_expires_for_five_rounds() {
    let mut credentials = KiroCredentials {
        id: Some(1),
        storage_revision: 7,
        refresh_token: Some(format!("refresh-{}", "x".repeat(192))),
        auth_method: Some("external_idp".to_string()),
        client_id: Some("client-a".to_string()),
        token_endpoint: Some("https://oauth.invalid/token".to_string()),
        ..Default::default()
    };
    let identity = RefreshAttemptIdentity::from_credentials(&credentials);
    assert_eq!(
        format!("{identity:?}"),
        "RefreshAttemptIdentity(<redacted>)"
    );
    let mut same_payload_new_revision = credentials.clone();
    same_payload_new_revision.storage_revision += 1;
    assert_eq!(
        identity,
        RefreshAttemptIdentity::from_credentials(&same_payload_new_revision),
        "metadata-only storage revision changes must not split the refresh wave"
    );
    let state = CredentialRefreshState::default();
    let failure = RefreshFailure::new(
        RefreshFailureStage::ResponseStatus,
        RefreshFailureKind::UpstreamUnavailable,
        Some(500),
        None,
        true,
    );
    let mut now = Instant::now();
    let mut previous_delay = StdDuration::ZERO;

    for round in 1..=5 {
        let delay = state
            .record_failure(identity, &failure, now, true)
            .expect("upstream failure is shareable");
        assert!(
            delay > previous_delay,
            "round {round}: exponential failure window must increase"
        );
        let replayed = state
            .replay_failure(&identity, now, true)
            .expect("same identity reuses the current typed result");
        assert_eq!(replayed.kind, RefreshFailureKind::UpstreamUnavailable);
        assert!(replayed.shared_failure_wave);
        assert!(state.replay_failure(&identity, now + delay, true).is_none());
        previous_delay = delay;
        now += delay;
    }
    assert!(previous_delay <= TOKEN_REFRESH_NEGATIVE_BACKOFF_MAX);
    let reset_delay = state
        .record_failure(
            identity,
            &failure,
            now + TOKEN_REFRESH_NEGATIVE_STREAK_RESET_AFTER + StdDuration::from_millis(1),
            true,
        )
        .expect("a later outage starts a fresh bounded wave");
    assert!(
        reset_delay <= TOKEN_REFRESH_NEGATIVE_BACKOFF_BASE,
        "widely spaced failures must not accumulate a permanent max backoff"
    );

    credentials.refresh_token = Some(format!("replacement-{}", "y".repeat(192)));
    let replacement = RefreshAttemptIdentity::from_credentials(&credentials);
    assert_ne!(identity, replacement);
    assert!(
        state
            .replay_failure(&replacement, Instant::now(), true)
            .is_none(),
        "a replaced refresh token must not inherit an old failure wave"
    );
    let config = Config::default();
    let second_config = Config::default();
    let proxy_a = ProxyConfig {
        url: "http://proxy-a.invalid:8080".to_string(),
        username: None,
        password: None,
    };
    let proxy_b = ProxyConfig {
        url: "http://proxy-b.invalid:8080".to_string(),
        username: None,
        password: None,
    };
    assert_ne!(
        RefreshAttemptIdentity::from_refresh_request(&credentials, &config, Some(&proxy_a)),
        RefreshAttemptIdentity::from_refresh_request(&credentials, &config, Some(&proxy_b)),
        "a transport change must bypass an old network failure wave"
    );
    assert_eq!(
        RefreshAttemptIdentity::from_refresh_request(&credentials, &config, None),
        RefreshAttemptIdentity::from_refresh_request(&credentials, &second_config, None),
        "default config must not randomize refresh identity"
    );

    let retry_after = RefreshFailure::new(
        RefreshFailureStage::ResponseStatus,
        RefreshFailureKind::RateLimited,
        Some(429),
        Some(StdDuration::from_secs(45)),
        true,
    );
    let retry_delay = state
        .record_failure(replacement, &retry_after, Instant::now(), true)
        .expect("committed 429 closes its immediate race");
    assert_eq!(retry_delay, StdDuration::from_secs(45));
    assert!(
        retry_delay <= TOKEN_REFRESH_NEGATIVE_RETRY_AFTER_MAX,
        "malicious Retry-After values remain bounded"
    );
    let malicious_retry_after = RefreshFailure::new(
        RefreshFailureStage::ResponseStatus,
        RefreshFailureKind::RateLimited,
        Some(429),
        Some(StdDuration::from_secs(3_600)),
        true,
    );
    assert_eq!(
        state
            .record_failure(replacement, &malicious_retry_after, Instant::now(), true)
            .expect("committed 429 remains shareable"),
        TOKEN_REFRESH_NEGATIVE_RETRY_AFTER_MAX
    );

    let other_credential_state = CredentialRefreshState::default();
    assert!(
        other_credential_state
            .replay_failure(&replacement, Instant::now(), true)
            .is_none(),
        "different credential slots never share a negative result"
    );

    let local_validation = RefreshFailure::new(
        RefreshFailureStage::Validation,
        RefreshFailureKind::InvalidConfiguration,
        None,
        None,
        false,
    );
    assert!(
        other_credential_state
            .record_failure(replacement, &local_validation, Instant::now(), true)
            .is_none(),
        "local validation failures are not negative-cached"
    );
    let pre_send_coordination = RefreshFailure::new(
        RefreshFailureStage::Coordination,
        RefreshFailureKind::Coordination,
        None,
        None,
        false,
    );
    assert!(
        other_credential_state
            .record_failure(replacement, &pre_send_coordination, Instant::now(), true)
            .is_none(),
        "pre-send local coordination failures must not be shared across callers or instances"
    );
    let pre_send_network = RefreshFailure::new(
        RefreshFailureStage::RequestSend,
        RefreshFailureKind::Network,
        None,
        None,
        false,
    );
    assert!(
        other_credential_state
            .record_failure(replacement, &pre_send_network, Instant::now(), true)
            .is_none(),
        "pre-send transport failures must not be shared before the upstream request is committed"
    );
    let pre_send_admission_rate_limit = RefreshFailure::new(
        RefreshFailureStage::Coordination,
        RefreshFailureKind::RateLimited,
        None,
        Some(StdDuration::from_secs(1)),
        false,
    );
    assert!(
        !refresh_failure_requires_health_action(&pre_send_admission_rate_limit),
        "local token-refresh admission rate limits must not mutate credential health"
    );
    assert!(
        other_credential_state
            .record_failure(
                replacement,
                &pre_send_admission_rate_limit,
                Instant::now(),
                true
            )
            .is_none(),
        "pre-send admission rate limits must not be negative-cached"
    );
    let committed_upstream_rate_limit = RefreshFailure::new(
        RefreshFailureStage::ResponseStatus,
        RefreshFailureKind::RateLimited,
        Some(429),
        Some(StdDuration::from_secs(1)),
        true,
    );
    assert!(
        refresh_failure_requires_health_action(&committed_upstream_rate_limit),
        "only committed upstream 429 describes credential health"
    );
    let direct_failure_at = Instant::now();
    assert!(
        other_credential_state
            .record_failure(replacement, &retry_after, direct_failure_at, false)
            .is_some(),
        "an admin/direct leader still closes duplicate OAuth sends"
    );
    assert!(
        other_credential_state
            .replay_failure(&replacement, direct_failure_at, false)
            .expect("another direct caller shares the wave")
            .shared_failure_wave,
        "a direct caller cannot consume scheduler health ownership"
    );
    assert!(
        !other_credential_state
            .replay_failure(&replacement, direct_failure_at, true)
            .expect("the first scheduler caller claims health ownership")
            .shared_failure_wave,
        "exactly one scheduler caller must apply the pending 429 health action"
    );
    assert!(
        other_credential_state
            .replay_failure(&replacement, direct_failure_at, true)
            .expect("later scheduler callers still share the wave")
            .shared_failure_wave
    );
}

#[tokio::test]
async fn api_key_token_path_does_not_allocate_oauth_refresh_state() {
    let credentials = KiroCredentials {
        id: Some(1),
        auth_method: Some("api_key".to_string()),
        kiro_api_key: Some("ksk_test_api_key_identity".to_string()),
        ..Default::default()
    };
    let manager = MultiTokenManager::new(
        Config::default(),
        vec![credentials.clone()],
        None,
        None,
        false,
    )
    .unwrap();

    let context = manager
        .try_ensure_token(1, &credentials, false)
        .await
        .expect("API key credentials bypass OAuth refresh");
    assert_eq!(context.token, "ksk_test_api_key_identity");
    assert!(manager.refresh_states.lock().is_empty());
}

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
fn admin_deferred_runtime_patch_advances_generation_and_ignores_older_replay() {
    let manager = MultiTokenManager::new(
        Config::default(),
        vec![test_access_token_credential(
            "admin-deferred-runtime",
            "Pro",
        )],
        None,
        None,
        false,
    )
    .unwrap();

    assert!(manager.enqueue_pending_runtime_mutation(
        1,
        PendingCredentialRuntimeMutation::ApiFailure {
            operation_id: uuid::Uuid::new_v4(),
            expected_generation: 0,
            last_used_at: Utc::now().to_rfc3339(),
        },
    ));
    manager
        .enqueue_admin_runtime_patch_for_recovery(
            1,
            uuid::Uuid::new_v4(),
            CredentialRuntimeStatePatch {
                failure_count: Some(0),
                refresh_failure_count: Some(0),
                disabled_reason: CredentialRuntimeDisabledReasonPatch::Clear,
                warmup_remaining: Some(7),
                credential_disabled: Some(false),
                expected_generation: Some(0),
                advance_generation: true,
                ..Default::default()
            },
        )
        .unwrap();

    {
        let entries = manager.entries.lock();
        let entry = entries.iter().find(|entry| entry.id == 1).unwrap();
        assert_eq!(entry.runtime_generation, 1);
        assert_eq!(entry.failure_count, 0);
        assert_eq!(entry.refresh_failure_count, 0);
        assert_eq!(entry.warmup_remaining, 7);
        assert!(!entry.credentials.disabled);
        assert!(entry.disabled_reason.is_none());
        assert!(entry.runtime_persistence_degraded);
        assert!(entry.runtime_persistence_quarantined);
    }
    assert_eq!(manager.runtime_mutation_backlog().0, 2);

    let older_replay = PersistedCredentialRuntimeMutation {
        state: CredentialRuntimeStateRow {
            failure_count: MAX_FAILURES_PER_CREDENTIAL,
            refresh_failure_count: MAX_FAILURES_PER_CREDENTIAL,
            disabled_reason: Some(DisabledReason::TooManyFailures.as_str().to_string()),
            warmup_remaining: 0,
            generation: 0,
            revision: 100,
        },
        credential_disabled: Some(true),
        applied: true,
    };
    {
        let mut entries = manager.entries.lock();
        let entry = entries.iter_mut().find(|entry| entry.id == 1).unwrap();
        MultiTokenManager::apply_persisted_runtime_mutation_to_entry(entry, &older_replay);
        assert_eq!(entry.runtime_generation, 1);
        assert_eq!(entry.failure_count, 0);
        assert_eq!(entry.refresh_failure_count, 0);
        assert_eq!(entry.warmup_remaining, 7);
        assert!(!entry.credentials.disabled);
        assert!(entry.disabled_reason.is_none());
    }
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

async fn test_redis_stores_with_shared_namespace(count: usize) -> Option<Vec<Arc<RedisStore>>> {
    let url = crate::storage::integration_test_url("KIRO_RS_TEST_REDIS_URL")?;
    let mut config = Config::default();
    config.redis.url = Some(url);
    config.redis.key_prefix = format!("kiro_rs:test:{}", uuid::Uuid::new_v4());
    let mut stores = Vec::with_capacity(count);
    for _ in 0..count {
        stores.push(Arc::new(RedisStore::connect(&config).await.unwrap()));
    }
    Some(stores)
}

async fn run_isolated_multi_redis_manager_fixture<F, Fut>(count: usize, body: F)
where
    F: FnOnce(Vec<Arc<RedisStore>>) -> Fut,
    Fut: Future<Output = ()>,
{
    let Some(stores) = test_redis_stores_with_shared_namespace(count).await else {
        eprintln!("跳过 Redis 多实例集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let cleanup_store = stores[0].clone();
    let outcome = AssertUnwindSafe(body(stores)).catch_unwind().await;
    crate::kiro::token_manager::drain_best_effort_storage_tasks(StdDuration::from_secs(5)).await;
    let cleanup = cleanup_store
        .delete_pattern_bounded("*", None)
        .await
        .unwrap();
    assert!(!cleanup.cancelled);
    assert!(!cleanup.pass_limit_reached);
    let after = cleanup_store
        .delete_pattern_bounded("*", None)
        .await
        .unwrap();
    assert_eq!(after.deleted_keys, 0);
    if let Err(payload) = outcome {
        std::panic::resume_unwind(payload);
    }
}

async fn run_isolated_redis_manager_fixture<F, Fut>(body: F)
where
    F: FnOnce(Arc<RedisStore>) -> Fut,
    Fut: Future<Output = ()>,
{
    let Some(store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let outcome = AssertUnwindSafe(body(store.clone())).catch_unwind().await;
    if test_redis_toxiproxy().is_some() {
        clear_test_redis_latency_toxic().await;
        set_test_redis_proxy_enabled(true).await;
    }
    crate::kiro::token_manager::drain_best_effort_storage_tasks(StdDuration::from_secs(5)).await;
    let cleanup = store.delete_pattern_bounded("*", None).await.unwrap();
    assert!(
        !cleanup.cancelled,
        "Redis manager fixture cleanup was cancelled"
    );
    assert!(
        !cleanup.pass_limit_reached,
        "Redis manager fixture cleanup did not converge"
    );
    let after = store.delete_pattern_bounded("*", None).await.unwrap();
    assert_eq!(
        after.deleted_keys, 0,
        "Redis manager fixture must leave its unique namespace empty"
    );
    if let Err(payload) = outcome {
        std::panic::resume_unwind(payload);
    }
}

#[derive(Debug, Default)]
struct UsageFaultWaveStats {
    attempted: u64,
    succeeded: u64,
    failed: u64,
    timed_out: u64,
}

fn spawn_usage_fault_wave(
    store: Arc<RedisStore>,
    scenario: String,
    workers: usize,
    records_per_worker: usize,
    start: Arc<tokio::sync::Barrier>,
) -> tokio::task::JoinHandle<UsageFaultWaveStats> {
    tokio::spawn(async move {
        let tasks = (0..workers)
            .map(|worker| {
                let store = store.clone();
                let scenario = scenario.clone();
                let start = start.clone();
                tokio::spawn(async move {
                    let mut stats = UsageFaultWaveStats::default();
                    start.wait().await;
                    for index in 0..records_per_worker {
                        let id =
                            format!("redis-joint-fault-{scenario}-worker-{worker}-record-{index}");
                        let record = sampled_request_rejection_usage_record(
                            &id,
                            "/cc/v1/messages",
                            Some("redis-joint-fault-key".to_string()),
                            "redis_joint_fault_validation",
                            "usage_writer",
                            http::StatusCode::SERVICE_UNAVAILABLE,
                            (worker * records_per_worker + index + 1) as u64,
                        );
                        stats.attempted += 1;
                        match tokio::time::timeout(
                            StdDuration::from_secs(3),
                            store.record_usage_summary(&record),
                        )
                        .await
                        {
                            Ok(Ok(true)) => stats.succeeded += 1,
                            Ok(Ok(false)) | Ok(Err(_)) => stats.failed += 1,
                            Err(_) => stats.timed_out += 1,
                        }
                    }
                    stats
                })
            })
            .collect::<Vec<_>>();
        let mut combined = UsageFaultWaveStats::default();
        for task in tasks {
            let stats = task.await.unwrap();
            combined.attempted += stats.attempted;
            combined.succeeded += stats.succeeded;
            combined.failed += stats.failed;
            combined.timed_out += stats.timed_out;
        }
        combined
    })
}

fn process_rss_kib_for_test() -> Option<u64> {
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    String::from_utf8(output.stdout).ok()?.trim().parse().ok()
}

fn open_fd_count_for_test() -> Option<usize> {
    std::fs::read_dir("/dev/fd")
        .ok()
        .map(|entries| entries.count())
}

async fn assert_capacity_breaker_fail_fast_without_redis_spin(
    manager: &MultiTokenManager,
    scenario: &str,
) {
    const CALLS: u64 = 128;
    let before = manager.scheduler_redis_breaker.stats_snapshot();
    let started = Instant::now();
    for _ in 0..CALLS {
        assert!(
            manager.acquire_in_flight_slot(1, 1).await.is_err(),
            "{scenario}: an open Redis breaker must fail closed"
        );
    }
    let elapsed = started.elapsed();
    let after = manager.scheduler_redis_breaker.stats_snapshot();
    assert_eq!(
        manager.local_pool_route_state(None).kind,
        LocalPoolRouteStateKind::SchedulerRedisDegraded,
        "{scenario}: breaker-open requests must not be reported as AllDisabled"
    );
    assert_eq!(
        after.admitted, before.admitted,
        "{scenario}: breaker-open calls must not reach Redis"
    );
    assert_eq!(
        after.failures, before.failures,
        "{scenario}: breaker-open calls must not manufacture Redis failures"
    );
    assert_eq!(
        after.fail_fast.saturating_sub(before.fail_fast),
        CALLS,
        "{scenario}: each caller should receive exactly one fail-fast admission result"
    );
    assert!(
        elapsed < StdDuration::from_millis(500),
        "{scenario}: breaker-open admission appears to spin: {elapsed:?}"
    );
    assert_eq!(manager.entries.lock()[0].in_flight_requests, 0);
}

async fn recover_capacity_breaker_five_times(
    manager: &MultiTokenManager,
    store: &RedisStore,
    scenario: &str,
) {
    let retry_after = manager
        .scheduler_redis_breaker
        .retry_after()
        .unwrap_or(SCHEDULER_REDIS_DEGRADED_BACKOFF_BASE);
    tokio::time::sleep(retry_after + StdDuration::from_millis(100)).await;
    for recovery in 1..=5 {
        let lease = manager
            .acquire_in_flight_slot(1, 1)
            .await
            .unwrap_or_else(|error| panic!("{scenario}: recovery {recovery}/5 failed: {error}"))
            .unwrap_or_else(|| panic!("{scenario}: recovery {recovery}/5 had no capacity"));
        drop(lease);
    }
    crate::kiro::token_manager::drain_best_effort_storage_tasks(StdDuration::from_secs(5)).await;
    assert!(
        !manager.scheduler_redis_breaker.is_degraded(),
        "{scenario}: breaker must close after healthy recovery probe"
    );
    tokio::time::timeout(StdDuration::from_secs(3), async {
        loop {
            match store.scheduler_state_for_credentials(&[1]).await {
                Ok(state)
                    if state
                        .get(&1)
                        .is_none_or(|state| state.in_flight_leases.is_empty()) =>
                {
                    break;
                }
                Ok(_) | Err(_) => tokio::time::sleep(StdDuration::from_millis(25)).await,
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{scenario}: recovered Redis leases did not drain"));
}

fn test_redis_toxiproxy() -> Option<(String, String)> {
    let api = std::env::var("KIRO_RS_TEST_TOXIPROXY_API").ok()?;
    let proxy = std::env::var("KIRO_RS_TEST_TOXIPROXY_NAME").ok()?;
    Some((api.trim_end_matches('/').to_string(), proxy))
}

async fn clear_test_redis_latency_toxic() -> bool {
    let Some((api, proxy)) = test_redis_toxiproxy() else {
        return false;
    };
    let response = reqwest::Client::new()
        .delete(format!(
            "{api}/proxies/{proxy}/toxics/scheduler-response-latency"
        ))
        .send()
        .await
        .expect("delete scheduler Redis latency toxic");
    assert!(
        response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND,
        "unexpected Toxiproxy delete status: {}",
        response.status()
    );
    true
}

async fn set_test_redis_latency_toxic(latency_ms: u64) -> bool {
    let Some((api, proxy)) = test_redis_toxiproxy() else {
        return false;
    };
    clear_test_redis_latency_toxic().await;
    let response = reqwest::Client::new()
        .post(format!("{api}/proxies/{proxy}/toxics"))
        .json(&serde_json::json!({
            "name": "scheduler-response-latency",
            "type": "latency",
            "stream": "downstream",
            "toxicity": 1.0,
            "attributes": {
                "latency": latency_ms,
                "jitter": 0,
            },
        }))
        .send()
        .await
        .expect("create scheduler Redis latency toxic");
    assert!(
        response.status().is_success(),
        "unexpected Toxiproxy create status: {}",
        response.status()
    );
    true
}

async fn set_test_redis_proxy_enabled(enabled: bool) -> bool {
    let Some((api, proxy)) = test_redis_toxiproxy() else {
        return false;
    };
    let response = reqwest::Client::new()
        .post(format!("{api}/proxies/{proxy}"))
        .json(&serde_json::json!({ "enabled": enabled }))
        .send()
        .await
        .expect("update scheduler Redis proxy state");
    assert!(
        response.status().is_success(),
        "unexpected Toxiproxy update status: {}",
        response.status()
    );
    true
}

fn test_fault_domain_toxiproxy(domain: &str) -> Option<(String, String)> {
    let domain = domain.to_ascii_uppercase();
    let api = std::env::var(format!("KIRO_RS_TEST_{domain}_TOXIPROXY_API")).ok()?;
    let proxy = std::env::var(format!("KIRO_RS_TEST_{domain}_TOXIPROXY_NAME")).ok()?;
    Some((api.trim_end_matches('/').to_string(), proxy))
}

async fn clear_fault_domain_latency(domain: &str) -> bool {
    let Some((api, proxy)) = test_fault_domain_toxiproxy(domain) else {
        return false;
    };
    let response = reqwest::Client::builder()
        .timeout(StdDuration::from_secs(2))
        .build()
        .unwrap()
        .delete(format!(
            "{api}/proxies/{proxy}/toxics/fault-domain-response-latency"
        ))
        .send()
        .await
        .expect("delete Redis fault-domain latency toxic");
    assert!(
        response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND,
        "unexpected fault-domain toxic delete status: {}",
        response.status()
    );
    true
}

async fn set_fault_domain_latency(domain: &str, latency_ms: u64) -> bool {
    let Some((api, proxy)) = test_fault_domain_toxiproxy(domain) else {
        return false;
    };
    clear_fault_domain_latency(domain).await;
    let response = reqwest::Client::builder()
        .timeout(StdDuration::from_secs(2))
        .build()
        .unwrap()
        .post(format!("{api}/proxies/{proxy}/toxics"))
        .json(&serde_json::json!({
            "name": "fault-domain-response-latency",
            "type": "latency",
            "stream": "downstream",
            "toxicity": 1.0,
            "attributes": { "latency": latency_ms, "jitter": 0 },
        }))
        .send()
        .await
        .expect("create Redis fault-domain latency toxic");
    assert!(
        response.status().is_success(),
        "unexpected fault-domain toxic create status: {}",
        response.status()
    );
    true
}

async fn set_fault_domain_proxy_enabled(domain: &str, enabled: bool) -> bool {
    let Some((api, proxy)) = test_fault_domain_toxiproxy(domain) else {
        return false;
    };
    let response = reqwest::Client::builder()
        .timeout(StdDuration::from_secs(2))
        .build()
        .unwrap()
        .post(format!("{api}/proxies/{proxy}"))
        .json(&serde_json::json!({ "enabled": enabled }))
        .send()
        .await
        .expect("update Redis fault-domain proxy state");
    assert!(
        response.status().is_success(),
        "unexpected fault-domain proxy status: {}",
        response.status()
    );
    true
}

async fn wait_fault_domain_store_healthy(store: &RedisStore, domain: &str) {
    tokio::time::timeout(StdDuration::from_secs(8), async {
        loop {
            if matches!(
                tokio::time::timeout(StdDuration::from_secs(1), store.ping()).await,
                Ok(Ok(()))
            ) {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{domain} Redis did not recover"));
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

async fn run_isolated_postgres_fixture<F, Fut>(body: F)
where
    F: FnOnce(Arc<PostgresStore>) -> Fut,
    Fut: Future<Output = ()>,
{
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };
    let outcome = AssertUnwindSafe(body(store.clone())).catch_unwind().await;
    store.drop_test_schema().await.unwrap();
    if let Err(payload) = outcome {
        std::panic::resume_unwind(payload);
    }
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
async fn steady_success_queues_stats_without_rewriting_unchanged_runtime_state() {
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL steady-success 测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };
    let mut credential = api_key_credential("steady-success");
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

    for _ in 0..5 {
        manager.report_success(1);
    }
    let steady_success_mutations: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM credential_runtime_mutations WHERE credential_id = $1 AND mutation_kind = 'success'",
    )
    .bind(1_i64)
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(
        steady_success_mutations, 0,
        "steady successes must not synchronously rewrite unchanged runtime state"
    );
    assert_eq!(
        manager
            .pending_stats_deltas
            .lock()
            .get(&1)
            .unwrap()
            .success_delta,
        5,
        "success accounting must remain lossless on the stats-delta path"
    );

    assert!(manager.report_failure(1));
    manager.report_success(1);
    let recovery_success_mutations: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM credential_runtime_mutations WHERE credential_id = $1 AND mutation_kind = 'success'",
    )
    .bind(1_i64)
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(
        recovery_success_mutations, 1,
        "a success that resets failure state must still use the authoritative runtime mutation"
    );
    let runtime = store.load_credential_runtime_state().await.unwrap();
    assert_eq!(runtime[&1].failure_count, 0);
    assert_eq!(runtime[&1].revision, 2);
    assert_eq!(
        manager
            .pending_stats_deltas
            .lock()
            .get(&1)
            .unwrap()
            .success_delta,
        6
    );

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stats_shutdown_drains_frozen_and_new_stats_with_multiple_runtime_rounds() {
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };
    let mut credential = api_key_credential("stats-shutdown-drain");
    credential.id = Some(1);
    store.save_credentials(&[credential.clone()]).await.unwrap();
    store
        .patch_credential_runtime_state(
            1,
            uuid::Uuid::new_v4(),
            &CredentialRuntimeStatePatch {
                warmup_remaining: Some(2),
                ..CredentialRuntimeStatePatch::default()
            },
        )
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
    assert!(manager.enqueue_pending_runtime_mutation(
        1,
        PendingCredentialRuntimeMutation::Success {
            operation_id: uuid::Uuid::new_v4(),
            expected_generation: 0,
            success_count: 1,
        },
    ));
    assert!(manager.enqueue_pending_runtime_mutation(
        1,
        PendingCredentialRuntimeMutation::ApiFailure {
            operation_id: uuid::Uuid::new_v4(),
            expected_generation: 0,
            last_used_at: "2026-01-01T00:00:00Z".to_string(),
        },
    ));
    assert!(manager.enqueue_pending_runtime_mutation(
        1,
        PendingCredentialRuntimeMutation::Success {
            operation_id: uuid::Uuid::new_v4(),
            expected_generation: 0,
            success_count: 1,
        },
    ));
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
    assert_eq!(runtime[&1].revision, 4);
    assert_eq!(runtime[&1].failure_count, 0);
    assert_eq!(runtime[&1].warmup_remaining, 0);

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
    for id in [2, 3] {
        store
            .patch_credential_runtime_state(
                id,
                uuid::Uuid::new_v4(),
                &CredentialRuntimeStatePatch {
                    warmup_remaining: Some(2),
                    ..CredentialRuntimeStatePatch::default()
                },
            )
            .await
            .unwrap();
    }
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
                    success_count: 1,
                },
            ));
        }
    }
    store.soft_delete_credential(1).await.unwrap();

    manager.flush_pending_runtime_mutations_with_budget(StdDuration::from_secs(2));

    {
        let pending = manager.pending_runtime_mutations.lock();
        assert_eq!(pending.len(), 1);
        let failed_queue = pending
            .get(&1)
            .expect("failed credential keeps pending success");
        assert_eq!(failed_queue.len(), 1);
        match failed_queue.front().unwrap() {
            PendingCredentialRuntimeMutation::Success { success_count, .. } => {
                assert_eq!(*success_count, 2);
            }
            other => panic!("expected coalesced success mutation, got {other:?}"),
        }
    }
    let states = store.load_credential_runtime_state().await.unwrap();
    assert_eq!(states[&2].revision, 2);
    assert_eq!(states[&2].warmup_remaining, 0);
    assert_eq!(states[&3].revision, 2);
    assert_eq!(states[&3].warmup_remaining, 0);
    {
        let entries = manager.entries.lock();
        let failed = entries.iter().find(|entry| entry.id == 1).unwrap();
        assert!(failed.runtime_persistence_degraded);
        assert!(!failed.runtime_persistence_quarantined);
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
            success_count: 1,
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

#[test]
fn startup_quota_guard_skips_fresh_exhausted_api_key_and_keeps_runtime_enabled() {
    let mut exhausted = api_key_credential("startup-quota-exhausted");
    exhausted.id = Some(1);
    exhausted.priority = 0;
    let mut healthy = api_key_credential("startup-quota-healthy");
    healthy.id = Some(2);
    healthy.priority = 1;
    let account_info = HashMap::from([(
        1,
        CredentialAccountInfoRow {
            remaining: 0.0,
            credit_remaining: 0.0,
            overage_status: Some("DISABLED".to_string()),
            checked_at: Utc::now().to_rfc3339(),
            ..Default::default()
        },
    )]);

    let manager = MultiTokenManager::new_with_stores_and_runtime_state_and_account_info(
        Config::default(),
        vec![exhausted, healthy],
        None,
        None,
        None,
        None,
        Some(account_info),
    )
    .unwrap();

    assert_eq!(manager.current_id(), 2);
    let entries = manager.entries.lock();
    let exhausted = entries.iter().find(|entry| entry.id == 1).unwrap();
    let healthy = entries.iter().find(|entry| entry.id == 2).unwrap();
    assert!(exhausted.account_quota_blocked);
    assert_eq!(
        exhausted.account_quota_block_reason.as_deref(),
        Some("fresh_account_info_remaining_and_credit_exhausted_overage_disabled")
    );
    assert!(!exhausted.disabled);
    assert!(!healthy.account_quota_blocked);
}

#[test]
fn quota_guard_ignores_stale_missing_non_disabled_and_oauth_account_snapshots() {
    let mut api_key = api_key_credential("quota-guard-matrix");
    api_key.id = Some(1);
    let oauth = KiroCredentials {
        id: Some(2),
        auth_method: Some("social".to_string()),
        refresh_token: Some("refresh".to_string()),
        ..Default::default()
    };
    let manager = MultiTokenManager::new(
        Config::default(),
        vec![api_key, oauth.clone()],
        None,
        None,
        false,
    )
    .unwrap();

    let stale = CredentialAccountInfoRow {
        remaining: 0.0,
        credit_remaining: 0.0,
        overage_status: Some("DISABLED".to_string()),
        checked_at: (Utc::now() - chrono::Duration::hours(1)).to_rfc3339(),
        ..Default::default()
    };
    let overage_enabled = CredentialAccountInfoRow {
        remaining: 0.0,
        credit_remaining: 0.0,
        overage_status: Some("ENABLED".to_string()),
        checked_at: Utc::now().to_rfc3339(),
        ..Default::default()
    };
    assert!(
        MultiTokenManager::account_info_quota_block_reason(
            &manager.entries.lock()[0],
            &stale,
            Utc::now()
        )
        .is_none()
    );
    assert!(
        MultiTokenManager::account_info_quota_block_reason(
            &manager.entries.lock()[0],
            &overage_enabled,
            Utc::now()
        )
        .is_none()
    );
    assert!(
        MultiTokenManager::account_info_quota_block_reason(
            &manager.entries.lock()[1],
            &CredentialAccountInfoRow {
                remaining: 0.0,
                credit_remaining: 0.0,
                overage_status: Some("DISABLED".to_string()),
                checked_at: Utc::now().to_rfc3339(),
                ..Default::default()
            },
            Utc::now()
        )
        .is_none(),
        "OAuth credentials must not use the API-key-only quota guard"
    );
    assert!(!manager.entries.lock()[0].account_quota_blocked);

    let exhausted = CredentialAccountInfoRow {
        remaining: 0.0,
        credit_remaining: 0.0,
        overage_status: Some("DISABLED".to_string()),
        checked_at: Utc::now().to_rfc3339(),
        ..Default::default()
    };
    manager.apply_loaded_account_info(&HashMap::from([(1, exhausted)]));
    assert!(manager.entries.lock()[0].account_quota_blocked);
    manager.apply_loaded_account_info(&HashMap::new());
    assert!(!manager.entries.lock()[0].account_quota_blocked);
}

#[test]
fn apply_account_info_snapshot_for_credential_refreshes_only_one_entry_and_reselects_current() {
    let mut first = api_key_credential("snapshot-refresh-first");
    first.id = Some(1);
    first.priority = 0;
    let mut second = api_key_credential("snapshot-refresh-second");
    second.id = Some(2);
    second.priority = 1;

    let manager = MultiTokenManager::new(Config::default(), vec![first, second], None, None, false)
        .expect("quota snapshot fixture");
    assert_eq!(manager.current_id(), 1);
    assert!(!manager.is_credential_account_quota_blocked(1));
    assert!(!manager.is_credential_account_quota_blocked(2));

    let exhausted = CredentialAccountInfoRow {
        remaining: 0.0,
        credit_remaining: 0.0,
        overage_status: Some("DISABLED".to_string()),
        checked_at: Utc::now().to_rfc3339(),
        ..Default::default()
    };
    assert!(manager.apply_account_info_snapshot_for_credential(1, &exhausted));
    assert!(manager.is_credential_account_quota_blocked(1));
    assert!(!manager.is_credential_account_quota_blocked(2));
    assert_eq!(manager.current_id(), 2);

    let healthy = CredentialAccountInfoRow {
        remaining: 10.0,
        credit_remaining: 10.0,
        overage_status: Some("ENABLED".to_string()),
        checked_at: Utc::now().to_rfc3339(),
        ..Default::default()
    };
    assert!(manager.apply_account_info_snapshot_for_credential(1, &healthy));
    assert!(!manager.is_credential_account_quota_blocked(1));
    assert_eq!(manager.current_id(), 1);
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
            success_count: 1,
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
async fn reload_account_info_quota_guard_reselects_healthy_credential() {
    run_isolated_postgres_fixture(|store| async move {
        let mut exhausted = api_key_credential("reload-quota-exhausted");
        exhausted.id = Some(1);
        exhausted.priority = 0;
        let mut healthy = api_key_credential("reload-quota-healthy");
        healthy.id = Some(2);
        healthy.priority = 1;
        store
            .save_credentials(&[exhausted.clone(), healthy.clone()])
            .await
            .unwrap();

        let manager = MultiTokenManager::new_with_stores(
            Config::default(),
            vec![exhausted, healthy],
            None,
            None,
            false,
            Some(store.clone()),
            None,
        )
        .unwrap();
        assert_eq!(manager.current_id(), 1);

        store
            .save_credential_account_info(
                1,
                &CredentialAccountInfoRow {
                    remaining: 0.0,
                    credit_remaining: 0.0,
                    overage_status: Some("DISABLED".to_string()),
                    checked_at: Utc::now().to_rfc3339(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert!(manager.reload_credentials_from_postgres().unwrap());
        assert_eq!(manager.current_id(), 2);
        assert!(manager.is_credential_account_quota_blocked(1));
        let entries = manager.entries.lock();
        let exhausted = entries.iter().find(|entry| entry.id == 1).unwrap();
        assert!(exhausted.account_quota_blocked);
        assert!(!exhausted.disabled);
        assert_eq!(
            exhausted.account_quota_block_reason.as_deref(),
            Some("fresh_account_info_remaining_and_credit_exhausted_overage_disabled")
        );
    })
    .await;
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
                success_count: 1,
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

#[tokio::test]
async fn auxiliary_focus_refresh_step_deadline_drops_future_owned_resources_before_returning() {
    struct DropProbe(Arc<std::sync::atomic::AtomicBool>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    for round in 1..=5 {
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let owned = DropProbe(dropped.clone());
        let deadline = tokio::time::Instant::now() + StdDuration::from_millis(10);
        let error = run_refresh_step_until("refresh resource cleanup test", deadline, async move {
            let _owned = owned;
            std::future::pending::<anyhow::Result<()>>().await
        })
        .await
        .unwrap_err();

        let typed = error
            .downcast_ref::<RefreshFailure>()
            .expect("refresh deadline must remain typed");
        assert_eq!(typed.stage, RefreshFailureStage::Internal, "round {round}");
        assert_eq!(typed.kind, RefreshFailureKind::Timeout, "round {round}");
        assert!(typed.send_committed, "round {round}");
        assert!(
            dropped.load(Ordering::Acquire),
            "round {round}: timeout must drop future-owned resources before returning"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn force_refresh_local_lock_wait_respects_coordination_deadline() {
    let credential = force_refresh_test_credential("http://127.0.0.1:1/token".to_string());
    let manager =
        MultiTokenManager::new(Config::default(), vec![credential], None, None, false).unwrap();
    let refresh_state = manager.refresh_state_for_credential(1);
    let _held = refresh_state.gate.lock().await;
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
    let typed = error
        .downcast_ref::<RefreshFailure>()
        .expect("local refresh lock timeout must remain typed");
    assert_eq!(typed.stage, RefreshFailureStage::Coordination);
    assert_eq!(typed.kind, RefreshFailureKind::Timeout);
    assert!(!typed.send_committed);
    assert!(started_at.elapsed() < StdDuration::from_millis(250));
    let snapshot = manager.snapshot();
    assert_eq!(snapshot.entries[0].refresh_failure_count, 0);
    assert!(!snapshot.entries[0].disabled);
    assert!(!snapshot.entries[0].cooled_down);
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
    let credential_id = inserted.id.expect("inserted credential must have an id");
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
    let peer_lock = redis
        .acquire_refresh_lock(credential_id, 30)
        .await
        .unwrap()
        .unwrap();

    let started_at = Instant::now();
    let result = manager
        .try_ensure_token_with_budgets(
            credential_id,
            &inserted,
            true,
            TokenRefreshBudgets {
                workflow: StdDuration::from_millis(250),
                coordination: StdDuration::from_millis(80),
                reconciliation: StdDuration::from_millis(50),
            },
        )
        .await;
    let elapsed = started_at.elapsed();
    let snapshot = manager.snapshot();
    let lock_released = redis
        .release_refresh_lock(credential_id, &peer_lock)
        .await
        .unwrap();
    store.drop_test_schema().await.unwrap();

    let error = match result {
        Ok(_) => panic!("peer refresh wait unexpectedly succeeded"),
        Err(error) => error,
    };
    let typed = error
        .downcast_ref::<RefreshFailure>()
        .expect("peer refresh coordination timeout must remain typed");
    assert_eq!(typed.stage, RefreshFailureStage::Coordination);
    assert_eq!(typed.kind, RefreshFailureKind::Timeout);
    assert!(!typed.send_committed);
    assert!(elapsed < StdDuration::from_millis(300));
    assert_eq!(snapshot.entries[0].refresh_failure_count, 0);
    assert!(!snapshot.entries[0].disabled);
    assert!(!snapshot.entries[0].cooled_down);
    assert!(lock_released);
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
    let error = result.unwrap_err();
    let typed = error
        .downcast_ref::<RefreshFailure>()
        .expect("pending refresh deadline must remain typed");
    assert_eq!(typed.kind, RefreshFailureKind::Timeout);
    assert!(typed.send_committed);
    assert!(started_at.elapsed() < StdDuration::from_secs(5));
    let snapshot = manager.snapshot();
    assert_eq!(snapshot.entries[0].refresh_failure_count, 0);
    assert!(!snapshot.entries[0].disabled);
    assert!(!snapshot.entries[0].cooled_down);
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
async fn ordinary_refresh_with_redis_without_postgres_uses_local_authority_and_releases_lock() {
    let Some(redis) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let (token_endpoint, _request_received, server) = spawn_force_refresh_token_endpoint().await;
    let mut credential = force_refresh_test_credential(token_endpoint);
    credential.expires_at = Some((Utc::now() - Duration::hours(1)).to_rfc3339());
    let refresh_credentials = credential.clone();
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

    let context = manager
        .try_ensure_token_with_budgets(
            1,
            &refresh_credentials,
            false,
            TokenRefreshBudgets {
                workflow: StdDuration::from_secs(5),
                coordination: StdDuration::from_secs(1),
                reconciliation: StdDuration::from_secs(1),
            },
        )
        .await
        .expect("Redis-only ordinary refresh must use the local credential snapshot");
    assert_eq!(context.token, "force-refreshed-access-token");
    assert_eq!(
        manager.entries.lock()[0]
            .credentials
            .access_token
            .as_deref(),
        Some("force-refreshed-access-token")
    );
    let next_lock = redis.acquire_refresh_lock(1, 30).await.unwrap().unwrap();
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
    let mut transaction = store.pool().begin().await.unwrap();
    sqlx::query("LOCK TABLE credential_runtime_state IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *transaction)
        .await
        .unwrap();

    let started_at = Instant::now();
    manager
        .force_refresh_token_for_with_budgets(
            1,
            TokenRefreshBudgets {
                workflow: StdDuration::from_millis(300),
                coordination: StdDuration::from_millis(50),
                reconciliation: StdDuration::from_millis(80),
            },
        )
        .await
        .unwrap();
    assert!(started_at.elapsed() < StdDuration::from_millis(600));
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
async fn ordinary_refresh_postgres_failure_is_typed_and_credential_health_neutral() {
    let (Some(store), Some(redis)) = (test_postgres_store().await, test_redis_store().await) else {
        eprintln!("跳过 PgSQL/Redis TokenManager 集成测试：未设置存储集成测试环境变量");
        return;
    };
    let (token_endpoint, request_received, server) = spawn_force_refresh_token_endpoint().await;
    let mut credential = force_refresh_test_credential(token_endpoint);
    credential.expires_at = Some((Utc::now() - Duration::hours(1)).to_rfc3339());
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

    let error = match manager.acquire_context(None).await {
        Ok(_) => panic!("ordinary refresh must fail when credential persistence fails"),
        Err(error) => error,
    };
    let typed = error
        .downcast_ref::<RefreshFailure>()
        .expect("refresh persistence failure must remain typed");
    assert_eq!(typed.stage, RefreshFailureStage::Persistence);
    assert_eq!(typed.kind, RefreshFailureKind::Persistence);
    assert!(!typed.send_committed);
    assert!(
        tokio::time::timeout(StdDuration::from_millis(50), request_received.notified())
            .await
            .is_err(),
        "PgSQL authority reload failure must fail before sending an OAuth refresh request"
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
        assert_eq!(entry.refresh_failure_count, 0);
        assert!(!entry.disabled);
        assert!(entry.cooldown_until.is_none());
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
            None,
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
async fn test_scheduler_state_sync_timeout_does_not_degrade_hot_path() {
    let manager = MultiTokenManager::new(
        Config::default(),
        vec![test_access_token_credential("token-1", "Pro")],
        None,
        None,
        false,
    )
    .unwrap();

    let result = manager
        .block_on_scheduler_redis_state_sync("测试 Redis 调度状态同步", async move {
            std::future::pending::<anyhow::Result<()>>().await
        });

    assert!(result.is_none());
    assert!(!manager.scheduler_redis_breaker.is_degraded());
    assert!(
        !manager.scheduler_redis_snapshot_breaker.is_degraded(),
        "snapshot/state-sync timeout must leave only the local snapshot stale; it must not create a fail-fast breaker window"
    );
    assert_eq!(
        manager.local_pool_route_state(None).kind,
        LocalPoolRouteStateKind::Ready,
        "a snapshot timeout must not route a locally dispatchable pool to external fallback"
    );
    assert_eq!(
        manager
            .scheduler_redis_snapshot_breaker
            .stats_snapshot()
            .failures,
        1
    );
}

#[test]
fn dispatch_wakeup_filters_self_and_routes_remote_scope_for_five_rounds() {
    for round in 0..5u64 {
        let manager = MultiTokenManager::new(
            Config::default(),
            vec![test_access_token_credential("token-1", "Pro")],
            None,
            None,
            false,
        )
        .unwrap();
        let self_payload = serde_json::json!({
            "kind": "dispatch_wakeup",
            "sourceInstanceId": manager.scheduler_instance_id.as_ref(),
            "credentialId": round + 1,
        })
        .to_string();
        for _ in 0..1_000 {
            assert!(!manager.notify_remote_dispatch_state_changed(&self_payload));
        }
        assert!(
            manager
                .scheduler_redis_dirty_credential_ids
                .lock()
                .is_empty()
        );
        assert_eq!(
            manager
                .scheduler_redis_full_sync_requested
                .load(Ordering::Acquire),
            0
        );

        let targeted = serde_json::json!({
            "kind": "dispatch_wakeup",
            "sourceInstanceId": format!("remote-{round}"),
            "credentialId": round + 10,
        })
        .to_string();
        assert!(manager.notify_remote_dispatch_state_changed(&targeted));
        assert_eq!(
            manager
                .scheduler_redis_dirty_credential_ids
                .lock()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![round + 10]
        );

        let broad = serde_json::json!({
            "kind": "dispatch_wakeup",
            "sourceInstanceId": format!("remote-broad-{round}"),
            "removed": 2,
        })
        .to_string();
        assert!(manager.notify_remote_dispatch_state_changed(&broad));
        assert_eq!(
            manager
                .scheduler_redis_full_sync_requested
                .load(Ordering::Acquire),
            1
        );
        assert!(!manager.notify_remote_dispatch_state_changed("not-json"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remote_scheduler_events_arriving_during_snapshot_are_chased_for_five_rounds() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis remote generation 测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    redis_store.set_scheduler_state_delay_millis(100);

    for round in 0..5u64 {
        let manager = MultiTokenManager::new_with_stores(
            Config::default(),
            vec![api_key_credential(&format!("remote-generation-{round}"))],
            None,
            None,
            false,
            None,
            Some(redis_store.clone()),
        )
        .unwrap();
        redis_store.reset_scheduler_state_round_trips();
        let started = Instant::now();
        let first = serde_json::json!({
            "kind": "dispatch_wakeup",
            "sourceInstanceId": format!("remote-a-{round}"),
            "credentialId": 1,
        })
        .to_string();
        assert!(manager.notify_remote_dispatch_state_changed(&first));

        tokio::time::timeout(StdDuration::from_millis(250), async {
            while redis_store.scheduler_state_round_trips() < 1 {
                tokio::time::sleep(StdDuration::from_millis(2)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("round {round}: first targeted snapshot did not start"));

        let second = serde_json::json!({
            "kind": "dispatch_wakeup",
            "sourceInstanceId": format!("remote-b-{round}"),
            "credentialId": 1,
        })
        .to_string();
        assert!(manager.notify_remote_dispatch_state_changed(&second));

        tokio::time::timeout(StdDuration::from_millis(700), async {
            loop {
                if redis_store.scheduler_state_round_trips() >= 2
                    && !manager
                        .scheduler_redis_sync_in_flight
                        .load(Ordering::Acquire)
                    && manager
                        .scheduler_redis_dirty_credential_ids
                        .lock()
                        .is_empty()
                {
                    break;
                }
                tokio::time::sleep(StdDuration::from_millis(2)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("round {round}: second event was not chased"));

        assert_eq!(
            redis_store.scheduler_state_round_trips(),
            2,
            "round {round}"
        );
        assert!(started.elapsed() < StdDuration::from_millis(700));
        assert!(!manager.scheduler_redis_breaker.is_degraded());
        assert!(!manager.scheduler_redis_snapshot_breaker.is_degraded());
    }

    redis_store.set_scheduler_state_delay_millis(0);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_scheduler_affinity_timeout_does_not_degrade_capacity_coordination() {
    let manager = MultiTokenManager::new(
        Config::default(),
        vec![test_access_token_credential("token-1", "Pro")],
        None,
        None,
        false,
    )
    .unwrap();

    let result = manager.block_on_scheduler_redis_affinity(
        "测试 Redis 会话粘性",
        std::future::pending::<anyhow::Result<()>>(),
    );

    assert!(result.is_none());
    assert!(
        !manager.scheduler_redis_breaker.is_degraded(),
        "会话粘性超时不得冻结凭据并发和全局队列准入"
    );
    assert!(
        manager.scheduler_redis_affinity_breaker.is_degraded(),
        "会话粘性自身应进入有限退避，避免 Redis 故障时每个请求重复打热路径"
    );

    let second_started = Instant::now();
    let skipped = manager.block_on_scheduler_redis_affinity("测试 Redis 会话粘性退避", async {
        Ok::<_, anyhow::Error>(())
    });
    assert!(skipped.is_none());
    assert!(second_started.elapsed() < StdDuration::from_millis(20));

    let route_state = manager.local_pool_route_state(None);
    assert_eq!(route_state.kind, LocalPoolRouteStateKind::Ready);
    let mut context = manager
        .acquire_context(None)
        .await
        .expect("会话粘性降级不得阻止本地账号取得并发槽");
    context.release_in_flight();
}

#[test]
fn scheduler_redis_backoff_is_stable_and_within_exponential_boundaries() {
    let breaker = SchedulerRedisBreaker::new(SchedulerRedisBreakerKind::Capacity, 1, 0xfeed_beef);
    let mut previous = StdDuration::ZERO;
    let mut previous_base = StdDuration::ZERO;
    for failure in 1..=8 {
        let base = SchedulerRedisBreaker::base_backoff_for_failure(failure);
        let first = breaker.backoff_for_failure(failure, failure as u64);
        let second = breaker.backoff_for_failure(failure, failure as u64);
        assert_eq!(first, second, "failure {failure}: jitter must be stable");
        assert!(
            first <= base,
            "failure {failure}: jitter must not exceed cap"
        );
        assert!(first >= base.saturating_mul(9) / 10);
        if base > previous_base {
            assert!(first > previous, "failure {failure}: backoff did not grow");
        }
        previous = first;
        previous_base = base;
    }
    assert!(previous <= SCHEDULER_REDIS_DEGRADED_BACKOFF_MAX);
}

#[tokio::test]
async fn scheduler_redis_saturation_does_not_open_breaker_c255_c256_c257_c512() {
    for contenders in [255usize, 256, 257, 512] {
        let breaker = SchedulerRedisBreaker::new(
            SchedulerRedisBreakerKind::Capacity,
            SCHEDULER_REDIS_MAX_IN_FLIGHT_OPERATIONS,
            contenders as u64,
        );
        let admitted = contenders.min(SCHEDULER_REDIS_MAX_IN_FLIGHT_OPERATIONS);
        let mut held = Vec::with_capacity(admitted);
        for _ in 0..admitted {
            held.push(
                breaker
                    .begin_until(
                        tokio::time::Instant::now() + StdDuration::from_secs(1),
                        StdDuration::from_secs(1),
                    )
                    .await
                    .expect("permit below the hard boundary"),
            );
        }
        for _ in admitted..contenders {
            assert_eq!(
                breaker
                    .begin_until(tokio::time::Instant::now(), SCHEDULER_REDIS_HOT_OP_TIMEOUT)
                    .await
                    .err(),
                Some(SchedulerRedisAdmissionError::LocalSchedulerOverloaded)
            );
        }
        assert_eq!(
            breaker.state.lock().phase,
            SchedulerRedisBreakerPhase::Closed,
            "c{contenders}: local saturation must not open the Redis breaker"
        );
        assert_eq!(
            breaker.stats_snapshot().local_saturated,
            contenders.saturating_sub(admitted) as u64
        );
        drop(held);
    }
}

#[tokio::test]
async fn scheduler_redis_semaphore_wait_uses_total_deadline_and_stays_not_started() {
    let breaker = SchedulerRedisBreaker::new(SchedulerRedisBreakerKind::Capacity, 1, 5);
    let held = breaker
        .begin_until(
            tokio::time::Instant::now() + StdDuration::from_secs(1),
            StdDuration::from_secs(1),
        )
        .await
        .expect("hold only permit");
    let started = Arc::new(AtomicBool::new(false));
    let operation_polled = Arc::new(AtomicBool::new(false));
    let started_for_hook = started.clone();
    let polled_for_future = operation_polled.clone();
    let began_at = Instant::now();
    let outcome = MultiTokenManager::execute_scheduler_redis_operation(
        breaker.clone(),
        SCHEDULER_REDIS_HOT_OP_TIMEOUT,
        "shared deadline test",
        move || started_for_hook.store(true, Ordering::Release),
        async move {
            polled_for_future.store(true, Ordering::Release);
            Ok::<_, anyhow::Error>(())
        },
    )
    .await;
    assert!(matches!(
        outcome,
        SchedulerRedisExecutionOutcome::NotStarted(
            SchedulerRedisAdmissionError::LocalSchedulerOverloaded
        )
    ));
    assert!(!started.load(Ordering::Acquire));
    assert!(!operation_polled.load(Ordering::Acquire));
    assert!(began_at.elapsed() >= SCHEDULER_REDIS_HOT_OP_TIMEOUT);
    assert!(began_at.elapsed() < SCHEDULER_REDIS_HOT_OP_TIMEOUT + StdDuration::from_millis(175));
    assert_eq!(
        breaker.state.lock().phase,
        SchedulerRedisBreakerPhase::Closed
    );
    assert_eq!(breaker.stats_snapshot().failures, 0);
    drop(held);
}

#[tokio::test]
async fn scheduler_redis_capacity_timeouts_open_only_after_consecutive_threshold() {
    let breaker = SchedulerRedisBreaker::new(SchedulerRedisBreakerKind::Capacity, 1, 0x5151);
    let timeout = StdDuration::from_millis(5);

    for attempt in 1..SCHEDULER_REDIS_TIMEOUT_FAILURES_TO_OPEN {
        let outcome = MultiTokenManager::execute_scheduler_redis_operation(
            breaker.clone(),
            timeout,
            "capacity timeout threshold test",
            || {},
            async move {
                tokio::time::sleep(timeout.saturating_mul(4)).await;
                Ok::<_, anyhow::Error>(())
            },
        )
        .await;
        assert!(
            matches!(
                outcome,
                SchedulerRedisExecutionOutcome::Failed {
                    commit_unknown: true
                }
            ),
            "attempt {attempt} should fail the request closed"
        );
        assert_eq!(
            breaker.state.lock().phase,
            SchedulerRedisBreakerPhase::Closed,
            "attempt {attempt} must not open the capacity breaker before the threshold"
        );
    }

    let threshold_outcome = MultiTokenManager::execute_scheduler_redis_operation(
        breaker.clone(),
        timeout,
        "capacity timeout threshold test",
        || {},
        async move {
            tokio::time::sleep(timeout.saturating_mul(4)).await;
            Ok::<_, anyhow::Error>(())
        },
    )
    .await;
    assert!(matches!(
        threshold_outcome,
        SchedulerRedisExecutionOutcome::Failed {
            commit_unknown: true
        }
    ));
    assert!(matches!(
        breaker.state.lock().phase,
        SchedulerRedisBreakerPhase::Open { .. }
    ));
    assert_eq!(
        breaker.stats_snapshot().failures,
        u64::from(SCHEDULER_REDIS_TIMEOUT_FAILURES_TO_OPEN)
    );
}

#[tokio::test]
async fn scheduler_redis_capacity_timeout_streak_resets_after_success() {
    let breaker = SchedulerRedisBreaker::new(SchedulerRedisBreakerKind::Capacity, 1, 0x5252);
    let timeout = StdDuration::from_millis(5);

    let timeout_outcome = MultiTokenManager::execute_scheduler_redis_operation(
        breaker.clone(),
        timeout,
        "capacity timeout reset test",
        || {},
        async move {
            tokio::time::sleep(timeout.saturating_mul(4)).await;
            Ok::<_, anyhow::Error>(())
        },
    )
    .await;
    assert!(matches!(
        timeout_outcome,
        SchedulerRedisExecutionOutcome::Failed {
            commit_unknown: true
        }
    ));
    assert_eq!(breaker.state.lock().consecutive_failures, 1);
    assert_eq!(
        breaker.state.lock().phase,
        SchedulerRedisBreakerPhase::Closed
    );

    let success = MultiTokenManager::execute_scheduler_redis_operation(
        breaker.clone(),
        timeout,
        "capacity timeout reset test",
        || {},
        async { Ok::<_, anyhow::Error>(()) },
    )
    .await;
    assert!(matches!(
        success,
        SchedulerRedisExecutionOutcome::Completed(())
    ));
    assert_eq!(breaker.state.lock().consecutive_failures, 0);

    let next_timeout = MultiTokenManager::execute_scheduler_redis_operation(
        breaker.clone(),
        timeout,
        "capacity timeout reset test",
        || {},
        async move {
            tokio::time::sleep(timeout.saturating_mul(4)).await;
            Ok::<_, anyhow::Error>(())
        },
    )
    .await;
    assert!(matches!(
        next_timeout,
        SchedulerRedisExecutionOutcome::Failed {
            commit_unknown: true
        }
    ));
    assert_eq!(breaker.state.lock().consecutive_failures, 1);
    assert_eq!(
        breaker.state.lock().phase,
        SchedulerRedisBreakerPhase::Closed,
        "a single timeout after a healthy success must not open the breaker"
    );
}

#[test]
fn scheduler_redis_response_errors_do_not_force_commit_unknown_recovery() {
    let response_error: anyhow::Error = redis::RedisError::from((
        redis::ErrorKind::ResponseError,
        "ERR",
        "WRONGTYPE Operation against a key holding the wrong kind of value".to_string(),
    ))
    .into();
    let timeout_error: anyhow::Error =
        std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out").into();
    let connection_refused: anyhow::Error =
        std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "connection refused").into();

    assert!(
        !scheduler_redis_failure_commit_unknown(&response_error),
        "WRONGTYPE and other deterministic Redis response errors must not be treated as commit-unknown"
    );
    assert!(
        scheduler_redis_failure_commit_unknown(&timeout_error),
        "timeouts must remain commit-unknown"
    );
    assert!(
        scheduler_redis_failure_commit_unknown(&connection_refused),
        "unknown non-Redis errors remain conservative"
    );
}

#[test]
fn stale_probe_success_cannot_clear_new_failure_for_10k_interleavings() {
    let breaker = SchedulerRedisBreaker::new(SchedulerRedisBreakerKind::Capacity, 1, 7);
    for generation in 0..10_000u64 {
        {
            let mut state = breaker.state.lock();
            state.failure_generation = generation.wrapping_add(1);
            state.consecutive_failures = 1;
            state.phase = SchedulerRedisBreakerPhase::Open {
                until: Instant::now() + StdDuration::from_secs(1),
            };
        }
        breaker.complete_success(generation, true);
        let state = breaker.state.lock();
        assert_eq!(state.failure_generation, generation.wrapping_add(1));
        assert!(matches!(
            state.phase,
            SchedulerRedisBreakerPhase::Open { .. }
        ));
    }
    assert_eq!(breaker.stats_snapshot().stale_successes, 10_000);
}

#[tokio::test]
async fn stale_probe_success_cannot_clear_new_failure_real_admissions() {
    let breaker = SchedulerRedisBreaker::new(SchedulerRedisBreakerKind::Capacity, 2, 9);
    let stale_success = breaker
        .begin_until(
            tokio::time::Instant::now() + StdDuration::from_secs(1),
            StdDuration::from_secs(1),
        )
        .await
        .expect("first closed admission");
    let failing = breaker
        .begin_until(
            tokio::time::Instant::now() + StdDuration::from_secs(1),
            StdDuration::from_secs(1),
        )
        .await
        .expect("second closed admission");
    let failure_generation = failing.failure_generation;
    failing.failure("generation fencing test", &anyhow::anyhow!("redis timeout"));
    stale_success.success();

    let state = breaker.state.lock();
    assert_eq!(state.failure_generation, failure_generation.wrapping_add(1));
    assert!(matches!(
        state.phase,
        SchedulerRedisBreakerPhase::Open { .. }
    ));
    assert_eq!(breaker.stats_snapshot().stale_successes, 1);
}

#[test]
fn stale_failures_cannot_extend_or_escalate_new_breaker_generation_for_10k_interleavings() {
    let breaker = SchedulerRedisBreaker::new(SchedulerRedisBreakerKind::Capacity, 1, 10);
    let error = anyhow::anyhow!("redis timeout");
    breaker.complete_failure(
        0,
        false,
        "first failure",
        SCHEDULER_REDIS_HOT_OP_TIMEOUT,
        &error,
    );
    let (generation, failures, phase) = {
        let state = breaker.state.lock();
        (
            state.failure_generation,
            state.consecutive_failures,
            state.phase,
        )
    };

    for _ in 0..10_000 {
        breaker.complete_failure(
            0,
            false,
            "stale failure",
            SCHEDULER_REDIS_HOT_OP_TIMEOUT,
            &error,
        );
    }

    let state = breaker.state.lock();
    assert_eq!(state.failure_generation, generation);
    assert_eq!(state.consecutive_failures, failures);
    assert_eq!(state.phase, phase);
    let stats = breaker.stats_snapshot();
    assert_eq!(stats.failures, 10_001);
    assert_eq!(stats.stale_failures, 10_000);
}

#[tokio::test]
async fn concurrent_failure_wave_opens_scheduler_breaker_once_without_backoff_amplification() {
    let breaker = SchedulerRedisBreaker::new(SchedulerRedisBreakerKind::Capacity, 256, 12);
    let mut admissions = Vec::with_capacity(256);
    for _ in 0..256 {
        admissions.push(
            breaker
                .begin_until(
                    tokio::time::Instant::now() + StdDuration::from_secs(1),
                    StdDuration::from_secs(1),
                )
                .await
                .expect("closed breaker must admit the bounded wave"),
        );
    }
    let admitted_generation = admissions[0].failure_generation;
    for admission in admissions {
        admission.failure("concurrent wave", &anyhow::anyhow!("redis timeout"));
    }

    let state = breaker.state.lock();
    assert_eq!(
        state.failure_generation,
        admitted_generation.wrapping_add(1)
    );
    assert_eq!(state.consecutive_failures, 1);
    assert!(matches!(
        state.phase,
        SchedulerRedisBreakerPhase::Open { .. }
    ));
    let stats = breaker.stats_snapshot();
    assert_eq!(stats.failures, 256);
    assert_eq!(stats.stale_failures, 255);
}

#[tokio::test]
async fn half_open_route_state_stays_degraded_until_the_single_probe_recovers() {
    let breaker = SchedulerRedisBreaker::new(SchedulerRedisBreakerKind::Capacity, 2, 11);
    breaker.state.lock().phase = SchedulerRedisBreakerPhase::Open {
        until: Instant::now() - StdDuration::from_millis(1),
    };
    assert!(breaker.is_degraded());
    let probe = breaker
        .begin_until(
            tokio::time::Instant::now() + StdDuration::from_secs(1),
            StdDuration::from_secs(1),
        )
        .await
        .expect("expired Open must admit one probe");
    assert!(probe.recovery_probe());
    assert_eq!(
        breaker.state.lock().phase,
        SchedulerRedisBreakerPhase::HalfOpen
    );
    assert_eq!(
        breaker
            .begin_until(
                tokio::time::Instant::now() + StdDuration::from_secs(1),
                StdDuration::from_secs(1),
            )
            .await
            .err(),
        Some(SchedulerRedisAdmissionError::BreakerOpen)
    );
    probe.success();
    assert_eq!(
        breaker.state.lock().phase,
        SchedulerRedisBreakerPhase::Closed
    );
    assert!(!breaker.is_degraded());
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
    assert!(!is_invalid_grant_response(
        reqwest::StatusCode::BAD_REQUEST,
        r#"{"error":"invalid_request","error_description":"contains invalid_grant text"}"#
    ));
    assert!(!is_invalid_grant_response(
        reqwest::StatusCode::BAD_REQUEST,
        r#"not-json \"invalid_grant\""#
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
    let error = result.unwrap_err();
    let typed = error
        .downcast_ref::<RefreshFailure>()
        .expect("API Key refresh rejection must remain a low-cardinality typed failure");
    assert_eq!(typed.stage, RefreshFailureStage::Validation);
    assert_eq!(typed.kind, RefreshFailureKind::InvalidConfiguration);
    assert_eq!(typed.status, None);
    assert_eq!(typed.retry_after, None);
    assert!(!typed.send_committed);
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
async fn postgres_failure_deferred_queues_before_runtime_flush() {
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };

    let mut credential = api_key_credential("ksk_deferred_failure_queue");
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

    assert!(manager.report_failure_deferred(1));
    let snapshot = manager.snapshot();
    let entry = snapshot.entries.iter().find(|entry| entry.id == 1).unwrap();
    assert_eq!(entry.failure_count, 1);
    assert!(!entry.disabled);
    assert_eq!(manager.pending_persistence_backlog().runtime_mutations, 1);

    let before_flush = store.load_credential_runtime_state().await.unwrap();
    assert_eq!(
        before_flush
            .get(&1)
            .map(|state| state.failure_count)
            .unwrap_or_default(),
        0
    );

    manager.save_stats();

    let after_flush = store.load_credential_runtime_state().await.unwrap();
    assert_eq!(after_flush.get(&1).unwrap().failure_count, 1);
    assert_eq!(manager.pending_persistence_backlog().runtime_mutations, 0);

    store.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_quota_deferred_persists_disable_on_runtime_flush() {
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };

    let mut credential = api_key_credential("ksk_deferred_quota_disable");
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

    assert!(!manager.report_quota_exhausted_deferred(1));
    let snapshot = manager.snapshot();
    let entry = snapshot.entries.iter().find(|entry| entry.id == 1).unwrap();
    assert!(entry.disabled);
    assert_eq!(
        entry.disabled_reason.as_deref(),
        Some(DisabledReason::QuotaExceeded.as_str())
    );
    assert_eq!(manager.pending_persistence_backlog().runtime_mutations, 1);
    assert!(
        manager
            .pending_runtime_mutations
            .lock()
            .get(&1)
            .and_then(|queue| queue.front())
            .is_some_and(PendingCredentialRuntimeMutation::requires_dispatch_quarantine)
    );

    let credentials_before_flush = store.load_credentials().await.unwrap();
    assert!(
        credentials_before_flush
            .iter()
            .any(|credential| credential.id == Some(1) && !credential.disabled)
    );

    manager.save_stats();

    let credentials_after_flush = store.load_credentials().await.unwrap();
    assert!(
        credentials_after_flush
            .iter()
            .any(|credential| credential.id == Some(1) && credential.disabled)
    );
    let runtime = store.load_credential_runtime_state().await.unwrap();
    assert_eq!(
        runtime.get(&1).unwrap().disabled_reason.as_deref(),
        Some(DisabledReason::QuotaExceeded.as_str())
    );
    assert_eq!(manager.pending_persistence_backlog().runtime_mutations, 0);

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

#[tokio::test(flavor = "multi_thread")]
async fn postgres_clean_success_reconciliation_is_rate_limited_per_credential() {
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };

    let mut credential = api_key_credential("ksk_success_reconcile_rate_limit");
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

    for _ in 0..8 {
        manager.report_success(1);
    }

    assert_eq!(
        manager.runtime_success_reconcile_probe_attempts(),
        1,
        "clean steady-state success must not issue one PgSQL runtime reconcile transaction per request"
    );

    store.drop_test_schema().await.unwrap();
}

#[test]
fn pending_success_runtime_mutations_coalesce_at_queue_tail() {
    let mut credential = api_key_credential("ksk_pending_success_coalesce");
    credential.id = Some(1);
    let manager = MultiTokenManager::new(Config::default(), vec![credential], None, None, false)
        .expect("construct token manager");

    manager.enqueue_pending_runtime_mutation(
        1,
        PendingCredentialRuntimeMutation::Success {
            operation_id: uuid::Uuid::new_v4(),
            expected_generation: 0,
            success_count: 1,
        },
    );
    manager.enqueue_pending_runtime_mutation(
        1,
        PendingCredentialRuntimeMutation::Success {
            operation_id: uuid::Uuid::new_v4(),
            expected_generation: 0,
            success_count: 1,
        },
    );
    assert_eq!(
        manager.runtime_mutation_backlog(),
        (1, 0),
        "repeated clean successes while PgSQL is degraded must not grow the pending queue"
    );
    {
        let pending = manager.pending_runtime_mutations.lock();
        let mutation = pending
            .get(&1)
            .and_then(|queue| queue.front())
            .expect("coalesced success must remain queued");
        match mutation {
            PendingCredentialRuntimeMutation::Success { success_count, .. } => {
                assert_eq!(*success_count, 2);
            }
            other => panic!("expected coalesced success mutation, got {other:?}"),
        }
    }

    manager.enqueue_pending_runtime_mutation(
        1,
        PendingCredentialRuntimeMutation::ApiFailure {
            operation_id: uuid::Uuid::new_v4(),
            expected_generation: 0,
            last_used_at: Utc::now().to_rfc3339(),
        },
    );
    manager.enqueue_pending_runtime_mutation(
        1,
        PendingCredentialRuntimeMutation::Success {
            operation_id: uuid::Uuid::new_v4(),
            expected_generation: 0,
            success_count: 1,
        },
    );
    manager.enqueue_pending_runtime_mutation(
        1,
        PendingCredentialRuntimeMutation::Success {
            operation_id: uuid::Uuid::new_v4(),
            expected_generation: 0,
            success_count: 1,
        },
    );
    assert_eq!(
        manager.runtime_mutation_backlog(),
        (3, 0),
        "a success after a pending failure must be preserved, then tail-coalesced again"
    );
    {
        let pending = manager.pending_runtime_mutations.lock();
        let tail = pending
            .get(&1)
            .and_then(|queue| queue.back())
            .expect("coalesced tail success must remain queued");
        match tail {
            PendingCredentialRuntimeMutation::Success { success_count, .. } => {
                assert_eq!(*success_count, 2);
            }
            other => panic!("expected coalesced success tail, got {other:?}"),
        }
    }
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
            success_count: 1,
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
        assert!(!entries[0].runtime_persistence_quarantined);
        assert!(!entries[0].disabled);
        assert_eq!(entries[0].failure_count, 1);
        assert_eq!(entries[0].runtime_revision, 1);
    }
    assert_eq!(manager.runtime_mutation_backlog().0, 2);

    manager.flush_pending_runtime_mutations();

    assert_eq!(manager.runtime_mutation_backlog(), (0, 0));
    {
        let entries = manager.entries.lock();
        assert!(!entries[0].runtime_persistence_degraded);
        assert!(!entries[0].runtime_persistence_quarantined);
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn postgres_pool_pressure_backlogs_non_terminal_success_without_quarantine_for_five_rounds() {
    run_isolated_postgres_fixture(|store| async move {
        let mut credential = api_key_credential("ksk_pool_pressure_non_terminal");
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

        for round in 1..=5 {
            {
                let mut entries = manager.entries.lock();
                let entry = entries.iter_mut().find(|entry| entry.id == 1).unwrap();
                entry.failure_count = 1;
                assert!(!entry.disabled, "round {round}");
            }

            let first_connection = store.pool().acquire().await.unwrap();
            let second_connection = store.pool().acquire().await.unwrap();
            let started_at = Instant::now();
            manager.report_success(1);
            let elapsed = started_at.elapsed();
            assert!(
                elapsed >= CREDENTIAL_PGSQL_SYNC_TIMEOUT.saturating_sub(StdDuration::from_millis(250)),
                "round {round}: real pool pressure should reach the bounded PgSQL timeout, elapsed={elapsed:?}"
            );
            assert!(
                elapsed < CREDENTIAL_PGSQL_SYNC_TIMEOUT + StdDuration::from_secs(3),
                "round {round}: PgSQL timeout must remain bounded, elapsed={elapsed:?}"
            );

            assert_eq!(manager.runtime_mutation_backlog(), (1, 0), "round {round}");
            {
                let entries = manager.entries.lock();
                let entry = entries.iter().find(|entry| entry.id == 1).unwrap();
                assert!(entry.runtime_persistence_degraded, "round {round}");
                assert!(!entry.runtime_persistence_quarantined, "round {round}");
                assert!(!entry.disabled, "round {round}");
                assert!(entry.disabled_reason.is_none(), "round {round}");
            }
            let route_state = manager.local_pool_route_state(None);
            assert_eq!(route_state.kind, LocalPoolRouteStateKind::Ready, "round {round}");
            assert_eq!(route_state.available, 1, "round {round}");
            assert_eq!(route_state.dispatchable, 1, "round {round}");

            drop(second_connection);
            drop(first_connection);
            manager.flush_pending_runtime_mutations();

            assert_eq!(manager.runtime_mutation_backlog(), (0, 0), "round {round}");
            {
                let entries = manager.entries.lock();
                let entry = entries.iter().find(|entry| entry.id == 1).unwrap();
                assert!(!entry.runtime_persistence_degraded, "round {round}");
                assert!(!entry.runtime_persistence_quarantined, "round {round}");
                assert!(!entry.disabled, "round {round}");
                assert_eq!(entry.failure_count, 0, "round {round}");
                assert_eq!(entry.runtime_revision, round, "round {round}");
            }
            let states = store.load_credential_runtime_state().await.unwrap();
            let state = states.get(&1).unwrap();
            assert_eq!(state.failure_count, 0, "round {round}");
            assert_eq!(state.revision, round, "round {round}");
            assert!(state.disabled_reason.is_none(), "round {round}");
            assert!(
                store
                    .load_credentials()
                    .await
                    .unwrap()
                    .iter()
                    .any(|credential| credential.id == Some(1) && !credential.disabled),
                "round {round}"
            );
        }

        crate::kiro::token_manager::drain_best_effort_storage_tasks(StdDuration::from_secs(1)).await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_deferred_success_does_not_wait_for_pgsql_pool_pressure_for_five_rounds() {
    run_isolated_postgres_fixture(|store| async move {
        let mut credential = api_key_credential("ksk_pool_pressure_terminal");
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

        for round in 1..=5 {
            {
                let mut entries = manager.entries.lock();
                let entry = entries.iter_mut().find(|entry| entry.id == 1).unwrap();
                entry.failure_count = 1;
                assert!(!entry.disabled, "round {round}");
            }

            let first_connection = store.pool().acquire().await.unwrap();
            let second_connection = store.pool().acquire().await.unwrap();
            let started_at = Instant::now();
            manager.report_success_for_session_with_latency_deferred(
                1,
                Some("claude-sonnet-4.5"),
                Some("terminal-session"),
                Some(StdDuration::from_millis(123)),
            );
            let elapsed = started_at.elapsed();
            assert!(
                elapsed < StdDuration::from_millis(500),
                "round {round}: terminal success must not wait for PgSQL pool pressure, elapsed={elapsed:?}"
            );

            assert_eq!(manager.runtime_mutation_backlog(), (1, 0), "round {round}");
            {
                let entries = manager.entries.lock();
                let entry = entries.iter().find(|entry| entry.id == 1).unwrap();
                assert!(entry.runtime_persistence_degraded, "round {round}");
                assert!(!entry.runtime_persistence_quarantined, "round {round}");
                assert!(!entry.disabled, "round {round}");
                assert!(entry.disabled_reason.is_none(), "round {round}");
                assert_eq!(entry.failure_count, 0, "round {round}");
                assert_eq!(entry.success_count, round, "round {round}");
            }
            let route_state = manager.local_pool_route_state(None);
            assert_eq!(route_state.kind, LocalPoolRouteStateKind::Ready, "round {round}");
            assert_eq!(route_state.available, 1, "round {round}");
            assert_eq!(route_state.dispatchable, 1, "round {round}");

            drop(second_connection);
            drop(first_connection);
            manager.flush_pending_runtime_mutations();

            assert_eq!(manager.runtime_mutation_backlog(), (0, 0), "round {round}");
            {
                let entries = manager.entries.lock();
                let entry = entries.iter().find(|entry| entry.id == 1).unwrap();
                assert!(!entry.runtime_persistence_degraded, "round {round}");
                assert!(!entry.runtime_persistence_quarantined, "round {round}");
                assert!(!entry.disabled, "round {round}");
                assert_eq!(entry.failure_count, 0, "round {round}");
                assert_eq!(entry.runtime_revision, round, "round {round}");
            }
        }

        crate::kiro::token_manager::drain_best_effort_storage_tasks(StdDuration::from_secs(1)).await;
    })
    .await;
}

#[test]
fn runtime_patch_quarantine_is_field_semantic_for_five_rounds() {
    for round in 1..=5 {
        for patch in [
            CredentialRuntimeStatePatch {
                failure_count: Some(0),
                expected_generation: Some(round),
                ..Default::default()
            },
            CredentialRuntimeStatePatch {
                refresh_failure_count: Some(0),
                expected_generation: Some(round),
                ..Default::default()
            },
            CredentialRuntimeStatePatch {
                warmup_remaining: Some(round as u32),
                expected_generation: Some(round),
                ..Default::default()
            },
            CredentialRuntimeStatePatch {
                last_used_at: Some(Utc::now().to_rfc3339()),
                expected_generation: Some(round),
                ..Default::default()
            },
        ] {
            assert!(
                !PendingCredentialRuntimeMutation::Patch {
                    operation_id: uuid::Uuid::new_v4(),
                    patch,
                }
                .requires_dispatch_quarantine(),
                "round {round}: health/stat patch must remain dispatchable"
            );
        }

        for patch in [
            CredentialRuntimeStatePatch {
                credential_disabled: Some(false),
                ..Default::default()
            },
            CredentialRuntimeStatePatch {
                disabled_reason: CredentialRuntimeDisabledReasonPatch::Clear,
                ..Default::default()
            },
            CredentialRuntimeStatePatch {
                disabled_reason: CredentialRuntimeDisabledReasonPatch::Set(
                    DisabledReason::Manual.as_str().to_string(),
                ),
                ..Default::default()
            },
            CredentialRuntimeStatePatch {
                advance_generation: true,
                ..Default::default()
            },
        ] {
            assert!(
                PendingCredentialRuntimeMutation::Patch {
                    operation_id: uuid::Uuid::new_v4(),
                    patch,
                }
                .requires_dispatch_quarantine(),
                "round {round}: dispatch-state patch must quarantine until persisted"
            );
        }
    }
}

#[test]
fn non_terminal_runtime_persistence_backlog_does_not_false_disable_pool_for_five_rounds() {
    for round in 1..=5 {
        let credentials = (0..40)
            .map(|idx| api_key_credential(&format!("ksk_runtime_backlog_{round}_{idx}")))
            .collect::<Vec<_>>();
        let manager =
            MultiTokenManager::new(Config::default(), credentials, None, None, false).unwrap();

        for id in 1..=40_u64 {
            let mutation = match id % 4 {
                0 => PendingCredentialRuntimeMutation::Success {
                    operation_id: uuid::Uuid::new_v4(),
                    expected_generation: 0,
                    success_count: 1,
                },
                1 => PendingCredentialRuntimeMutation::ApiFailure {
                    operation_id: uuid::Uuid::new_v4(),
                    expected_generation: 0,
                    last_used_at: Utc::now().to_rfc3339(),
                },
                2 => PendingCredentialRuntimeMutation::RefreshFailure {
                    operation_id: uuid::Uuid::new_v4(),
                    expected_generation: 0,
                    last_used_at: Utc::now().to_rfc3339(),
                },
                _ => PendingCredentialRuntimeMutation::Patch {
                    operation_id: uuid::Uuid::new_v4(),
                    patch: CredentialRuntimeStatePatch {
                        refresh_failure_count: Some(0),
                        expected_generation: Some(0),
                        ..Default::default()
                    },
                },
            };
            assert!(manager.enqueue_pending_runtime_mutation(id, mutation));
        }

        let state = manager.local_pool_route_state(None);
        assert_eq!(state.kind, LocalPoolRouteStateKind::Ready, "round {round}");
        assert_eq!(state.total, 40, "round {round}");
        assert_eq!(state.available, 40, "round {round}");
        assert_eq!(state.dispatchable, 40, "round {round}");
        assert_eq!(manager.runtime_mutation_backlog().0, 40, "round {round}");
        {
            let entries = manager.entries.lock();
            assert!(
                entries
                    .iter()
                    .all(|entry| entry.runtime_persistence_degraded),
                "round {round}"
            );
            assert!(
                entries
                    .iter()
                    .all(|entry| !entry.runtime_persistence_quarantined && !entry.disabled),
                "round {round}"
            );
        }

        assert!(manager.enqueue_pending_runtime_mutation(
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
        assert!(manager.enqueue_pending_runtime_mutation(
            2,
            PendingCredentialRuntimeMutation::Patch {
                operation_id: uuid::Uuid::new_v4(),
                patch: CredentialRuntimeStatePatch {
                    disabled_reason: CredentialRuntimeDisabledReasonPatch::Set(
                        DisabledReason::Manual.as_str().to_string(),
                    ),
                    credential_disabled: Some(true),
                    advance_generation: true,
                    ..Default::default()
                },
            },
        ));
        {
            let entries = manager.entries.lock();
            for id in [1_u64, 2] {
                let entry = entries.iter().find(|entry| entry.id == id).unwrap();
                assert!(entry.runtime_persistence_degraded, "round {round}, id {id}");
                assert!(
                    entry.runtime_persistence_quarantined,
                    "round {round}, id {id}"
                );
                assert!(entry.disabled, "round {round}, id {id}");
            }
            assert_eq!(
                entries.iter().filter(|entry| entry.disabled).count(),
                2,
                "round {round}"
            );
        }
        let partially_quarantined = manager.local_pool_route_state(None);
        assert_eq!(
            partially_quarantined.kind,
            LocalPoolRouteStateKind::Ready,
            "round {round}"
        );
        assert_eq!(partially_quarantined.available, 38, "round {round}");
    }
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn postgres_admin_capacity_update_defers_runtime_patch_during_recovery() {
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };

    let mut config = Config::default();
    config.credential_warmup_requests = 7;
    let mut credential = api_key_credential("ksk_admin_capacity_deferred");
    credential.id = Some(1);
    store.save_credentials(&[credential.clone()]).await.unwrap();
    let manager = MultiTokenManager::new_with_stores(
        config,
        vec![credential],
        None,
        None,
        false,
        Some(store.clone()),
        None,
    )
    .unwrap();

    assert!(manager.enqueue_pending_runtime_mutation(
        1,
        PendingCredentialRuntimeMutation::ApiFailure {
            operation_id: uuid::Uuid::new_v4(),
            expected_generation: 0,
            last_used_at: Utc::now().to_rfc3339(),
        },
    ));
    assert_eq!(manager.runtime_mutation_backlog().0, 1);

    manager
        .set_credential_max_concurrent_requests(1, Some(20))
        .unwrap();

    {
        let entries = manager.entries.lock();
        let entry = entries.iter().find(|entry| entry.id == 1).unwrap();
        assert_eq!(entry.credentials.max_concurrent_requests, Some(20));
        assert_eq!(entry.warmup_remaining, 7);
        assert_eq!(entry.runtime_generation, 1);
        assert!(entry.runtime_persistence_degraded);
        assert!(entry.runtime_persistence_quarantined);
    }
    assert_eq!(manager.runtime_mutation_backlog().0, 2);
    assert!(
        store
            .load_credentials()
            .await
            .unwrap()
            .iter()
            .any(|credential| {
                credential.id == Some(1) && credential.max_concurrent_requests == Some(20)
            })
    );

    manager.save_stats();
    assert_eq!(manager.runtime_mutation_backlog(), (0, 0));
    let states = store.load_credential_runtime_state().await.unwrap();
    let state = states.get(&1).unwrap();
    assert_eq!(state.generation, 1);
    assert_eq!(state.warmup_remaining, 7);
    {
        let entries = manager.entries.lock();
        let entry = entries.iter().find(|entry| entry.id == 1).unwrap();
        assert!(!entry.runtime_persistence_degraded);
        assert!(!entry.runtime_persistence_quarantined);
        assert_eq!(entry.warmup_remaining, 7);
    }

    store.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn postgres_admin_reset_enable_defers_runtime_patch_during_recovery() {
    let Some(store) = test_postgres_store().await else {
        eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };

    let mut credential = api_key_credential("ksk_admin_reset_deferred");
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

    assert!(manager.enqueue_pending_runtime_mutation(
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
    assert_eq!(manager.runtime_mutation_backlog().0, 1);

    manager.reset_and_enable(1).unwrap();

    {
        let entries = manager.entries.lock();
        let entry = entries.iter().find(|entry| entry.id == 1).unwrap();
        assert_eq!(entry.failure_count, 0);
        assert_eq!(entry.refresh_failure_count, 0);
        assert_eq!(entry.runtime_generation, 1);
        assert!(!entry.credentials.disabled);
        assert!(entry.disabled_reason.is_none());
        assert!(entry.runtime_persistence_degraded);
        assert!(entry.runtime_persistence_quarantined);
    }
    assert_eq!(manager.runtime_mutation_backlog().0, 2);

    manager.save_stats();
    assert_eq!(manager.runtime_mutation_backlog(), (0, 0));
    {
        let entries = manager.entries.lock();
        let entry = entries.iter().find(|entry| entry.id == 1).unwrap();
        assert_eq!(entry.runtime_generation, 1);
        assert_eq!(entry.failure_count, 0);
        assert_eq!(entry.refresh_failure_count, 0);
        assert!(!entry.runtime_persistence_degraded);
        assert!(!entry.runtime_persistence_quarantined);
        assert!(!entry.disabled);
        assert!(entry.disabled_reason.is_none());
    }
    let states = store.load_credential_runtime_state().await.unwrap();
    let state = states.get(&1).unwrap();
    assert_eq!(state.generation, 1);
    assert_eq!(state.failure_count, 0);
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
fn report_failure_deferred_matches_local_scheduler_semantics() {
    let manager = MultiTokenManager::new(
        Config::default(),
        vec![KiroCredentials::default(), KiroCredentials::default()],
        None,
        None,
        false,
    )
    .unwrap();

    assert!(manager.report_failure_deferred(1));
    assert!(manager.report_failure_deferred(1));
    assert_eq!(manager.available_count(), 2);

    assert!(manager.report_failure_deferred(1));
    assert_eq!(manager.available_count(), 1);
    let snapshot = manager.snapshot();
    assert_eq!(snapshot.current_id, 2);
    let first = snapshot.entries.iter().find(|entry| entry.id == 1).unwrap();
    assert!(first.disabled);
    assert_eq!(first.failure_count, MAX_FAILURES_PER_CREDENTIAL);
    assert_eq!(
        first.disabled_reason.as_deref(),
        Some(DisabledReason::TooManyFailures.as_str())
    );

    assert!(manager.report_failure_deferred(2));
    assert!(manager.report_failure_deferred(2));
    assert!(!manager.report_failure_deferred(2));
    assert_eq!(manager.available_count(), 0);
}

#[test]
fn deferred_terminal_disable_variants_update_local_state_without_store() {
    let manager = MultiTokenManager::new(
        Config::default(),
        vec![
            KiroCredentials::default(),
            KiroCredentials::default(),
            KiroCredentials::default(),
        ],
        None,
        None,
        false,
    )
    .unwrap();

    assert!(manager.report_quota_exhausted_deferred(1));
    let quota = manager
        .snapshot()
        .entries
        .into_iter()
        .find(|entry| entry.id == 1)
        .unwrap();
    assert!(quota.disabled);
    assert_eq!(
        quota.disabled_reason.as_deref(),
        Some(DisabledReason::QuotaExceeded.as_str())
    );

    assert!(
        manager
            .report_risk_controlled_outcome_deferred(
                2,
                CredentialRiskControlReason::TemporarilySuspended,
                "TEMPORARILY_SUSPENDED"
            )
            .can_retry_local()
    );
    let risk = manager
        .snapshot()
        .entries
        .into_iter()
        .find(|entry| entry.id == 2)
        .unwrap();
    assert!(risk.disabled);
    assert_eq!(
        risk.disabled_reason.as_deref(),
        Some(DisabledReason::TemporarilySuspended.as_str())
    );

    assert!(!manager.report_refresh_token_invalid_deferred(3));
    let invalid = manager
        .snapshot()
        .entries
        .into_iter()
        .find(|entry| entry.id == 3)
        .unwrap();
    assert!(invalid.disabled);
    assert_eq!(
        invalid.disabled_reason.as_deref(),
        Some(DisabledReason::InvalidRefreshToken.as_str())
    );
    assert_eq!(manager.available_count(), 0);
}

#[test]
fn profile_arn_deferred_updates_local_state_without_store() {
    let manager = MultiTokenManager::new(
        Config::default(),
        vec![KiroCredentials::default()],
        None,
        None,
        false,
    )
    .unwrap();

    manager
        .update_credential_profile_arn_deferred(
            1,
            Some("arn:aws:codewhisperer:us-east-1:123456789012:profile/test".to_string()),
        )
        .unwrap();
    assert!(
        manager
            .snapshot()
            .entries
            .iter()
            .find(|entry| entry.id == 1)
            .unwrap()
            .has_profile_arn
    );

    manager
        .update_credential_profile_arn_deferred(1, None)
        .unwrap();
    assert!(
        !manager
            .snapshot()
            .entries
            .iter()
            .find(|entry| entry.id == 1)
            .unwrap()
            .has_profile_arn
    );
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

#[test]
fn finite_dispatch_queue_lease_covers_actual_wait_and_unlimited_wait_renews() {
    for round in 1..=5 {
        let mut config = Config::default();
        config.credential_dispatch_max_wait_secs = 120;
        let manager = MultiTokenManager::new(config, Vec::new(), None, None, false).unwrap();

        let configured =
            dispatch_queue_lease_policy(manager.dispatch_max_wait(AcquireMode::WaitForCapacity));
        assert_eq!(configured.ttl_secs, 180, "round {round}");
        assert!(!configured.renewal_required, "round {round}");

        let longer_override = dispatch_queue_lease_policy(
            manager.dispatch_max_wait(AcquireMode::WaitForCapacityMax(StdDuration::from_secs(300))),
        );
        assert_eq!(longer_override.ttl_secs, 360, "round {round}");
        assert!(!longer_override.renewal_required, "round {round}");

        let fractional_override = dispatch_queue_lease_policy(Some(StdDuration::from_millis(1501)));
        assert_eq!(fractional_override.ttl_secs, 62, "round {round}");
        assert!(!fractional_override.renewal_required, "round {round}");

        let unlimited = dispatch_queue_lease_policy(None);
        assert_eq!(unlimited.ttl_secs, 60, "round {round}");
        assert!(unlimited.renewal_required, "round {round}");
    }
}

#[tokio::test]
async fn test_multi_token_manager_acquire_context_auto_recovers_all_disabled() {
    let config = Config::default();
    let cred1 = test_access_token_credential("t1", "Pro");
    let cred2 = test_access_token_credential("t2", "Pro");

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
async fn test_multi_token_manager_acquire_context_balanced_request_excludes_bad_configuration() {
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
    let snapshot = manager.snapshot();
    let invalid = snapshot.entries.iter().find(|entry| entry.id == 1).unwrap();
    assert!(!invalid.disabled);
    assert!(!invalid.cooled_down);
    assert_eq!(invalid.refresh_failure_count, 0);
    assert!(invalid.last_error_kind.is_none());
}

#[tokio::test]
async fn test_all_invalid_refresh_configurations_are_request_bounded_and_health_neutral() {
    for round in 1..=5 {
        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();

        let mut first = KiroCredentials::default();
        first.refresh_token = Some("bad".to_string());
        let mut second = KiroCredentials::default();
        second.refresh_token = Some("also-bad".to_string());

        let manager =
            MultiTokenManager::new(config, vec![first, second], None, None, false).unwrap();
        let started = Instant::now();
        let error = tokio::time::timeout(StdDuration::from_secs(2), manager.acquire_context(None))
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "round {round}: all invalid refresh configurations must finish within the request bound"
                )
            })
            .err()
            .unwrap();
        let elapsed = started.elapsed();

        let typed = error
            .downcast_ref::<RefreshFailure>()
            .expect("invalid refresh configuration remains typed");
        assert_eq!(
            typed.stage,
            RefreshFailureStage::Validation,
            "round {round}"
        );
        assert_eq!(
            typed.kind,
            RefreshFailureKind::InvalidConfiguration,
            "round {round}"
        );
        assert!(!typed.send_committed, "round {round}");

        let clients = manager.refresh_client_cache_snapshot();
        assert_eq!(clients.entries, 0, "round {round}");
        assert_eq!(clients.builds, 0, "round {round}");
        assert_eq!(clients.hits, 0, "round {round}");
        assert_eq!(clients.misses, 0, "round {round}");
        assert_eq!(clients.saturated, 0, "round {round}");
        let concurrency = manager.auxiliary_concurrency_snapshot();
        assert_eq!(concurrency.in_flight, 0, "round {round}");
        assert_eq!(concurrency.peak_in_flight, 0, "round {round}");

        let snapshot = manager.snapshot();
        assert_eq!(snapshot.available, 2, "round {round}");
        assert!(
            snapshot.entries.iter().all(|entry| !entry.disabled),
            "round {round}"
        );
        assert!(
            snapshot
                .entries
                .iter()
                .all(|entry| entry.refresh_failure_count == 0),
            "round {round}"
        );
        assert!(
            snapshot.entries.iter().all(|entry| !entry.cooled_down),
            "round {round}"
        );
        assert!(
            snapshot
                .entries
                .iter()
                .all(|entry| entry.last_error_kind.is_none()),
            "round {round}"
        );
        eprintln!(
            "INVALID_REFRESH_CONFIG_BOUND round={round} elapsed_us={}",
            elapsed.as_micros()
        );
    }
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

    let binding_reads_before = manager
        .request_binding_snapshot_reads
        .load(Ordering::Acquire);
    let mut fallback = manager
        .acquire_context_for_session(None, Some("sticky-full"), &empty)
        .await
        .unwrap();
    assert_eq!(
        manager
            .request_binding_snapshot_reads
            .load(Ordering::Acquire)
            .saturating_sub(binding_reads_before),
        1,
        "slot reselects must reuse one request-scoped binding snapshot"
    );

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
        runtime_persistence_quarantined: false,
        disabled: false,
        disabled_reason: None,
        account_quota_blocked: false,
        account_quota_block_reason: None,
        success_count: 0,
        total_selection_count: 100,
        last_used_at: None,
        cooldown_until: None,
        cooldown_reason: None,
        model_cooldowns: HashMap::new(),
        rate_limit_available_at: None,
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
        runtime_persistence_quarantined: false,
        disabled: false,
        disabled_reason: None,
        account_quota_blocked: false,
        account_quota_block_reason: None,
        success_count: 0,
        total_selection_count: 0,
        last_used_at: None,
        cooldown_until: None,
        cooldown_reason: None,
        model_cooldowns: HashMap::new(),
        rate_limit_available_at: None,
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
    assert!(
        manager
            .report_risk_controlled_outcome(
                4,
                CredentialRiskControlReason::TemporarilySuspended,
                "TEMPORARILY_SUSPENDED"
            )
            .can_retry_local()
    );
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
    first.release_in_flight();

    let state = manager.local_pool_route_state(None);
    assert_eq!(state.kind, LocalPoolRouteStateKind::AllCoolingDown);
    assert_eq!(state.rate_limit_blocked, 1);
    assert!(state.retry_after_secs.is_some());
}

#[tokio::test]
async fn test_rate_limiter_allows_idle_burst_up_to_rpm_capacity() {
    let mut config = Config::default();
    config.credential_rpm = Some(50);
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

    let mut leases = Vec::new();
    for _ in 0..50 {
        leases.push(manager.acquire_context(None).await.unwrap());
    }

    let snapshot = manager.snapshot();
    assert_eq!(snapshot.global_in_flight_requests, 50);
    assert!(snapshot.entries[0].rate_limited);

    for lease in &mut leases {
        lease.release_in_flight();
    }
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
async fn test_credential_rpm_override_limits_when_global_unlimited() {
    let mut config = Config::default();
    config.credential_rpm = None;

    let mut cred = test_access_token_credential("t1", "Pro");
    cred.rpm = Some(1);

    let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();
    let mut first = manager.acquire_context(None).await.unwrap();
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
async fn priority_mode_respects_warmup_candidate_share() {
    let mut config = Config::default();
    config.load_balancing_mode = "priority".to_string();
    config.credential_warmup_selection_percent = 0;
    config.credential_warmup_max_selection_percent = 0;

    let mut ready = test_access_token_credential("ready", "Pro");
    ready.priority = 10;
    let mut warming = test_access_token_credential("warming", "Pro");
    warming.priority = 0;

    let manager = MultiTokenManager::new(config, vec![ready, warming], None, None, false).unwrap();
    manager
        .set_warmup_remaining(2, 5)
        .expect("mark second credential warming");

    let mut ctx = manager
        .acquire_context(None)
        .await
        .expect("ready credential should be selected");
    assert_eq!(
        ctx.id, 1,
        "priority mode must not bypass warmup share to select the lower-priority warming account"
    );
    ctx.release_in_flight();
}

#[test]
fn credential_capacity_updates_reset_warmup_remaining() {
    let mut config = Config::default();
    config.credential_warmup_requests = 7;
    let manager = MultiTokenManager::new(
        config,
        vec![test_access_token_credential("capacity", "Pro")],
        None,
        None,
        false,
    )
    .unwrap();

    assert_eq!(manager.snapshot().entries[0].warmup_remaining, 0);
    manager.set_credential_rpm(1, Some(30)).unwrap();
    assert_eq!(manager.snapshot().entries[0].warmup_remaining, 7);

    manager.report_success(1);
    assert_eq!(manager.snapshot().entries[0].warmup_remaining, 6);
    manager
        .set_credential_max_concurrent_requests(1, Some(20))
        .unwrap();
    assert_eq!(manager.snapshot().entries[0].warmup_remaining, 7);
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
    tokio::time::timeout(StdDuration::from_secs(1), async {
        while manager.queued_requests.load(Ordering::Acquire) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("等待请求应在硬期限内进入唯一队列名额");
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_request_keeps_admission_wait_deadline_when_runtime_config_changes_for_five_rounds()
{
    for round in 1..=5 {
        let mut config = Config::default();
        config.credential_max_concurrent_requests = 1;
        config.dispatch_global_max_concurrent_requests = 1;
        config.dispatch_max_queued_requests = 1;
        config.credential_dispatch_max_wait_secs = 1;
        let manager = Arc::new(
            MultiTokenManager::new(
                config,
                vec![api_key_credential(&format!(
                    "stable-queue-deadline-{round}"
                ))],
                None,
                None,
                false,
            )
            .unwrap(),
        );

        let mut first = manager.acquire_context(None).await.unwrap();
        let waiting_manager = manager.clone();
        let started_at = Instant::now();
        let mut waiting = tokio::spawn(async move { waiting_manager.acquire_context(None).await });
        tokio::time::timeout(StdDuration::from_secs(1), async {
            while manager.queued_requests.load(Ordering::Acquire) != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("round {round}: request should enter the queue"));

        manager
            .update_runtime_config(|config| config.credential_dispatch_max_wait_secs = 5)
            .unwrap();
        let joined = match tokio::time::timeout(StdDuration::from_millis(1_750), &mut waiting).await
        {
            Ok(joined) => joined,
            Err(_) => {
                waiting.abort();
                let _ = waiting.await;
                first.release_in_flight();
                panic!("round {round}: runtime config update extended an admitted request deadline")
            }
        };
        let acquire_error = match joined {
            Ok(Err(error)) => error,
            Ok(Ok(mut context)) => {
                context.release_in_flight();
                first.release_in_flight();
                panic!("round {round}: waiter unexpectedly acquired capacity")
            }
            Err(error) => {
                first.release_in_flight();
                panic!("round {round}: waiter task failed: {error}")
            }
        };
        let elapsed = started_at.elapsed();
        assert!(acquire_error.to_string().contains("max_wait_secs=1"));
        assert!(
            (StdDuration::from_millis(800)..StdDuration::from_millis(1_750)).contains(&elapsed),
            "round {round}: frozen one-second deadline elapsed={elapsed:?}"
        );
        assert_eq!(manager.queued_requests.load(Ordering::Acquire), 0);
        assert_eq!(manager.available_count(), 1);
        assert!(
            manager
                .snapshot()
                .entries
                .iter()
                .all(|entry| !entry.disabled)
        );
        first.release_in_flight();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forty_by_fifteen_with_global_five_hundred_queues_without_disabling_for_five_rounds() {
    const CREDENTIALS: usize = 40;
    const PER_CREDENTIAL: u32 = 15;
    const GLOBAL_LIMIT: u32 = 500;

    for round in 1..=5 {
        let credentials = (0..CREDENTIALS)
            .map(|idx| api_key_credential(&format!("ksk_c500_{round}_{idx}")))
            .collect::<Vec<_>>();
        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();
        config.credential_rpm = Some(60);
        config.credential_max_concurrent_requests = PER_CREDENTIAL;
        config.dispatch_global_max_concurrent_requests = GLOBAL_LIMIT;
        config.dispatch_max_queued_requests = 1;
        config.credential_dispatch_max_wait_secs = 1;
        let manager = Arc::new(
            MultiTokenManager::new(config.clone(), credentials, None, None, false).unwrap(),
        );

        let mut held = Vec::with_capacity(GLOBAL_LIMIT as usize);
        for idx in 0..GLOBAL_LIMIT as usize {
            let id = (idx % CREDENTIALS) as u64 + 1;
            held.push(
                manager
                    .acquire_in_flight_slot(id, 1)
                    .await
                    .unwrap_or_else(|err| panic!("round {round}, slot {idx}: {err}"))
                    .unwrap_or_else(|| panic!("round {round}, slot {idx} should fit")),
            );
        }

        let full = manager.local_pool_route_state(None);
        assert_eq!(
            full.kind,
            LocalPoolRouteStateKind::CapacityFull,
            "round {round}"
        );
        assert_eq!(full.total, CREDENTIALS, "round {round}");
        assert_eq!(full.available, CREDENTIALS, "round {round}");
        assert_eq!(
            full.global_in_flight_requests, GLOBAL_LIMIT,
            "round {round}"
        );
        assert_eq!(full.rate_limit_blocked, 0, "round {round}");
        assert_eq!(full.concurrency_blocked, CREDENTIALS, "round {round}");
        assert_eq!(manager.available_count(), CREDENTIALS, "round {round}");

        let waiting_manager = Arc::clone(&manager);
        let waiting = tokio::spawn(async move { waiting_manager.acquire_context(None).await });
        tokio::time::timeout(StdDuration::from_secs(1), async {
            while manager.queued_requests.load(Ordering::Acquire) != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("round {round}: request should enter the bounded queue"));
        assert!(!waiting.is_finished(), "round {round}");
        assert_eq!(manager.available_count(), CREDENTIALS, "round {round}");

        drop(held.pop().unwrap());
        let mut replacement = tokio::time::timeout(StdDuration::from_secs(1), waiting)
            .await
            .unwrap_or_else(|_| panic!("round {round}: released capacity should wake waiter"))
            .unwrap_or_else(|err| panic!("round {round}: waiter task failed: {err}"))
            .unwrap_or_else(|err| panic!("round {round}: waiter acquire failed: {err}"));
        replacement.release_in_flight();
        drop(held);
        let drained = manager.snapshot();
        assert_eq!(drained.global_in_flight_requests, 0, "round {round}");
        assert_eq!(drained.queued_requests, 0, "round {round}");
        assert!(
            drained.entries.iter().all(|entry| !entry.disabled),
            "round {round}"
        );

        config.dispatch_global_max_concurrent_requests = 0;
        config.dispatch_max_queued_requests = 0;
        let credentials = (0..CREDENTIALS)
            .map(|idx| api_key_credential(&format!("ksk_c501_{round}_{idx}")))
            .collect::<Vec<_>>();
        let unlimited_global =
            MultiTokenManager::new(config, credentials, None, None, false).unwrap();
        let mut below_per_credential_total = Vec::with_capacity(GLOBAL_LIMIT as usize);
        for idx in 0..GLOBAL_LIMIT as usize {
            let id = (idx % CREDENTIALS) as u64 + 1;
            below_per_credential_total.push(
                unlimited_global
                    .acquire_in_flight_slot(id, 1)
                    .await
                    .unwrap_or_else(|err| panic!("round {round}, unlimited slot {idx}: {err}"))
                    .unwrap_or_else(|| panic!("round {round}, unlimited slot {idx} should fit")),
            );
        }
        let still_ready = unlimited_global.local_pool_route_state(None);
        assert_eq!(
            still_ready.kind,
            LocalPoolRouteStateKind::Ready,
            "round {round}"
        );
        assert_eq!(
            still_ready.global_in_flight_requests, GLOBAL_LIMIT,
            "round {round}"
        );
        assert!(still_ready.dispatchable > 0, "round {round}");
        let mut extra = tokio::time::timeout(
            StdDuration::from_millis(500),
            unlimited_global.acquire_context(None),
        )
        .await
        .unwrap_or_else(|_| panic!("round {round}: slot 501 should not queue"))
        .unwrap_or_else(|err| panic!("round {round}: slot 501 failed: {err}"));
        extra.release_in_flight();
        drop(below_per_credential_total);
        assert_eq!(
            unlimited_global.snapshot().global_in_flight_requests,
            0,
            "round {round}"
        );
    }
}

#[tokio::test]
async fn test_fail_fast_global_capacity_full_returns_without_queueing_for_five_rounds() {
    for round in 1..=5 {
        let mut config = Config::default();
        config.dispatch_global_max_concurrent_requests = 1;
        config.dispatch_max_queued_requests = 10;

        let first_cred = test_access_token_credential("first", "Pro");
        let second_cred = test_access_token_credential("second", "Pro");
        let manager =
            MultiTokenManager::new(config, vec![first_cred, second_cred], None, None, false)
                .unwrap();

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
            "round {round}: fail-fast 模式全局容量满应直接返回容量错误，实际: {err}"
        );
        let snapshot = manager.snapshot();
        assert_eq!(snapshot.global_in_flight_requests, 1, "round {round}");
        assert_eq!(snapshot.queued_requests, 0, "round {round}");
        first.release_in_flight();
        assert_eq!(
            manager.snapshot().global_in_flight_requests,
            0,
            "round {round}"
        );
    }
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
async fn test_local_pool_route_state_reports_capacity_full_without_queueing_for_five_rounds() {
    for round in 1..=5 {
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
        assert_eq!(ready.kind, LocalPoolRouteStateKind::Ready, "round {round}");
        assert_eq!(ready.dispatchable, 1, "round {round}");

        let mut ctx = manager.acquire_context(None).await.unwrap();
        let full = manager.local_pool_route_state(None);
        assert_eq!(
            full.kind,
            LocalPoolRouteStateKind::CapacityFull,
            "round {round}"
        );
        assert_eq!(full.dispatchable, 0, "round {round}");
        assert_eq!(full.concurrency_blocked, 1, "round {round}");
        assert_eq!(full.queued_requests, 0, "round {round}");

        ctx.release_in_flight();
        let ready_again = manager.local_pool_route_state(None);
        assert_eq!(
            ready_again.kind,
            LocalPoolRouteStateKind::Ready,
            "round {round}"
        );
        assert_eq!(ready_again.dispatchable, 1, "round {round}");
    }
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
    config.credential_rpm = Some(60);
    let manager = MultiTokenManager::new(
        config,
        vec![test_access_token_credential("first", "Pro")],
        None,
        None,
        false,
    )
    .unwrap();

    for _ in 0..60 {
        let mut ctx = manager.acquire_context(None).await.unwrap();
        ctx.release_in_flight();
    }
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
    let blocked = manager.local_pool_route_state(None);
    assert_eq!(blocked.kind, LocalPoolRouteStateKind::ProxyBlocked);
    assert_eq!(blocked.proxy_blocked, 1);

    manager.proxy_resources.lock().get_mut(&7).unwrap().enabled = true;
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
async fn test_fail_fast_slot_race_reselects_other_available_credential_for_five_rounds() {
    for round in 1..=5 {
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
            MultiTokenManager::new(config, vec![first_cred, second_cred], None, None, false)
                .unwrap();
        let mut first = manager.acquire_context(None).await.unwrap();
        assert_eq!(first.id, 1, "round {round}");

        let mut second = manager
            .acquire_context_for_session_with_mode(
                None,
                None,
                &HashSet::new(),
                AcquireMode::FailFastOnCapacity,
                1,
            )
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "round {round}: fail-fast should reselect another credential when the selected slot is full: {error}"
                )
            });

        assert_eq!(second.id, 2, "round {round}");
        let snapshot = manager.snapshot();
        assert_eq!(snapshot.entries[0].in_flight_requests, 1, "round {round}");
        assert_eq!(snapshot.entries[1].in_flight_requests, 1, "round {round}");
        assert_eq!(snapshot.queued_requests, 0, "round {round}");

        first.release_in_flight();
        second.release_in_flight();
        assert_eq!(
            manager.snapshot().global_in_flight_requests,
            0,
            "round {round}"
        );
    }
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

    let manager_a = Arc::new(
        MultiTokenManager::new_with_stores(
            config.clone(),
            vec![api_key_credential("a")],
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
            vec![api_key_credential("a")],
            None,
            None,
            false,
            None,
            Some(redis_store),
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
        leases.iter().filter(|lease| lease.is_some()).count(),
        1,
        "健康 Redis 下超过两个并发请求也必须严格执行跨 manager 的单槽限制"
    );
    drop(leases);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn redis_two_instance_connections_preserve_lease_queue_and_rpm_authority_for_five_rounds() {
    run_isolated_multi_redis_manager_fixture(4, |stores| async move {
        for round in 1..=5 {
            stores[0].delete_pattern_bounded("*", None).await.unwrap();
            let mut config = Config::default();
            config.credential_max_concurrent_requests = 4;
            config.credential_in_flight_lease_max_secs = 1;
            config.credential_dispatch_max_wait_secs = 2;
            let credentials = vec![api_key_credential(&format!("two-instance-{round}"))];
            let managers = stores
                .iter()
                .map(|store| {
                    Arc::new(
                        MultiTokenManager::new_with_stores(
                            config.clone(),
                            credentials.clone(),
                            None,
                            None,
                            false,
                            None,
                            Some(store.clone()),
                        )
                        .unwrap(),
                    )
                })
                .collect::<Vec<_>>();

            let start = Arc::new(tokio::sync::Barrier::new(2));
            let acquire_a = {
                let manager = managers[0].clone();
                let start = start.clone();
                tokio::spawn(async move {
                    start.wait().await;
                    manager.acquire_in_flight_slot(1, 1).await.unwrap().unwrap()
                })
            };
            let acquire_b = {
                let manager = managers[1].clone();
                let start = start.clone();
                tokio::spawn(async move {
                    start.wait().await;
                    manager.acquire_in_flight_slot(1, 1).await.unwrap().unwrap()
                })
            };
            let lease_a = acquire_a.await.unwrap();
            let lease_b = acquire_b.await.unwrap();
            assert_ne!(
                lease_a.id(),
                lease_b.id(),
                "round {round}: independent instance lease IDs must not collide"
            );
            let state = stores[2]
                .scheduler_state_for_credentials(&[1])
                .await
                .unwrap();
            let ids = state[&1]
                .in_flight_leases
                .iter()
                .map(|lease| lease.id)
                .collect::<HashSet<_>>();
            assert_eq!(ids, HashSet::from([lease_a.id(), lease_b.id()]));

            let lease_b_id = lease_b.id();
            drop(lease_a);
            assert!(
                managers[0]
                    .drain_scheduler_redis_releases(StdDuration::from_secs(3))
                    .await
            );
            let state = stores[2]
                .scheduler_state_for_credentials(&[1])
                .await
                .unwrap();
            assert_eq!(
                state[&1]
                    .in_flight_leases
                    .iter()
                    .map(|lease| lease.id)
                    .collect::<Vec<_>>(),
                vec![lease_b_id],
                "round {round}: releasing instance A must not remove instance B's lease"
            );

            let crashed_lease_id = stores[2]
                .next_in_flight_lease_id()
                .await
                .unwrap()
                .max(1);
            let crashed_lease = stores[2]
                .acquire_dispatch_lease(
                    1,
                    crashed_lease_id,
                    4,
                    0,
                    1,
                    Some(StdDuration::from_secs(1)),
                    InFlightKind::Api.as_str(),
                )
                .await
                .unwrap();
            assert!(crashed_lease.is_some(), "round {round}");
            assert_ne!(crashed_lease_id, lease_b_id);
            for _ in 0..5 {
                tokio::time::sleep(StdDuration::from_millis(250)).await;
                lease_b.touch();
            }
            crate::kiro::token_manager::drain_best_effort_storage_tasks(
                StdDuration::from_secs(2),
            )
            .await;

            let restarted_lease = managers[3]
                .acquire_in_flight_slot(1, 1)
                .await
                .unwrap()
                .unwrap();
            let restarted_lease_id = restarted_lease.id();
            let state = stores[0]
                .scheduler_state_for_credentials(&[1])
                .await
                .unwrap();
            let ids = state[&1]
                .in_flight_leases
                .iter()
                .map(|lease| lease.id)
                .collect::<HashSet<_>>();
            assert!(ids.contains(&lease_b_id), "round {round}");
            assert!(ids.contains(&restarted_lease_id), "round {round}");
            assert!(
                !ids.contains(&crashed_lease_id),
                "round {round}: an instance-disappearance lease must expire before restart admission"
            );
            assert_eq!(ids.len(), 2, "round {round}");

            let queue_calls = (0..16)
                .map(|index| {
                    let store = stores[index % 2].clone();
                    async move {
                        let lease_id = format!("two-instance-queue-{round}-{index}");
                        let admitted = store
                            .try_enter_dispatch_queue(&lease_id, 4, 60)
                            .await
                            .unwrap();
                        (store, lease_id, admitted)
                    }
                })
                .collect::<Vec<_>>();
            let queue_results = futures::future::join_all(queue_calls).await;
            let admitted = queue_results
                .iter()
                .filter(|(_, _, admitted)| *admitted)
                .count();
            assert_eq!(
                admitted, 4,
                "round {round}: two instances must share one dispatch queue limit"
            );
            for (store, lease_id, admitted) in queue_results {
                if admitted {
                    assert!(store.leave_dispatch_queue(&lease_id).await.unwrap());
                }
            }
            assert_eq!(
                stores[0]
                    .global_capacity_state()
                    .await
                    .unwrap()
                    .queued_requests,
                0,
                "round {round}"
            );

            let rpm_calls = (0..8)
                .map(|index| {
                    let store = stores[index % 2].clone();
                    async move {
                        store
                            .bump_rate_limit_available_at(1, StdDuration::from_millis(10))
                            .await
                            .unwrap()
                    }
                })
                .collect::<Vec<_>>();
            let mut rate_deadlines = futures::future::join_all(rpm_calls).await;
            rate_deadlines.sort_unstable();
            assert_eq!(
                rate_deadlines.iter().copied().collect::<HashSet<_>>().len(),
                rate_deadlines.len(),
                "round {round}: shared RPM reservations must be unique"
            );
            assert!(
                rate_deadlines
                    .windows(2)
                    .all(|pair| pair[1].saturating_sub(pair[0]) >= 10),
                "round {round}: shared RPM reservations must serialize one interval each"
            );
            stores[1].clear_rate_limit(1).await.unwrap();

            drop(lease_b);
            drop(restarted_lease);
            for manager in &managers {
                assert!(
                    manager
                        .drain_scheduler_redis_releases(StdDuration::from_secs(3))
                        .await
                );
            }
            let state = stores[0]
                .scheduler_state_for_credentials(&[1])
                .await
                .unwrap();
            assert!(
                state
                    .get(&1)
                    .is_none_or(|state| state.in_flight_leases.is_empty()),
                "round {round}: active leases must drain after both instances release"
            );
            eprintln!(
                "redis-two-instance round={round} lease_ids_unique=true crash_ttl_recovered=true queue_admitted={admitted}/16 rpm_reservations={} final_leases=0",
                rate_deadlines.len(),
            );
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn redis_backed_rpm_reservation_blocks_third_cross_instance_selection() {
    run_isolated_multi_redis_manager_fixture(2, |stores| async move {
        let mut config = Config::default();
        config.credential_rpm = None;
        config.credential_max_concurrent_requests = 8;
        config.dispatch_max_queued_requests = 4;
        config.credential_dispatch_max_wait_secs = 1;

        let mut credential = api_key_credential("shared-rpm-reservation");
        credential.id = Some(1);
        credential.rpm = Some(2);

        let manager_a = MultiTokenManager::new_with_stores(
            config.clone(),
            vec![credential.clone()],
            None,
            None,
            false,
            None,
            Some(stores[0].clone()),
        )
        .unwrap();
        let manager_b = MultiTokenManager::new_with_stores(
            config,
            vec![credential],
            None,
            None,
            false,
            None,
            Some(stores[1].clone()),
        )
        .unwrap();

        let mut first = manager_a.acquire_context(None).await.unwrap();
        first.release_in_flight();
        let mut second = manager_b.acquire_context(None).await.unwrap();
        second.release_in_flight();
        assert!(
            stores[0].get_rate_limit_available_at(1).await.unwrap().is_some(),
            "second cross-instance selection should establish the shared Redis RPM deadline"
        );

        let started = Instant::now();
        let error = match manager_a.acquire_context(None).await {
            Ok(mut context) => {
                context.release_in_flight();
                panic!("third cross-instance selection unexpectedly acquired context")
            }
            Err(error) => error.to_string(),
        };
        assert!(
            started.elapsed() >= StdDuration::from_millis(900),
            "third request should wait for its bounded dispatch deadline, not fail open immediately: {error}"
        );
        assert!(
            error.contains("凭据 RPM 限制"),
            "third cross-instance selection should be rejected by the shared RPM reservation, actual: {error}"
        );
        assert!(manager_a.drain_scheduler_redis_releases(StdDuration::from_secs(3)).await);
        assert!(manager_b.drain_scheduler_redis_releases(StdDuration::from_secs(3)).await);
        let state = stores[0]
            .scheduler_state_for_credentials(&[1])
            .await
            .unwrap();
        assert!(
            state
                .get(&1)
                .is_none_or(|state| state.in_flight_leases.is_empty()),
            "rate-limited third selection must release its provisional in-flight lease"
        );
    })
    .await;
}

#[path = "manager_refresh_cluster_tests.rs"]
mod refresh_cluster_tests;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redis_backed_in_flight_limit_does_not_fail_open_while_degraded() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };

    let mut config = Config::default();
    config.credential_max_concurrent_requests = 1;
    let manager = Arc::new(
        MultiTokenManager::new_with_stores(
            config,
            vec![api_key_credential("degraded")],
            None,
            None,
            false,
            None,
            Some(redis_store.clone()),
        )
        .unwrap(),
    );
    manager.scheduler_redis_breaker.state.lock().phase = SchedulerRedisBreakerPhase::Open {
        until: Instant::now() + StdDuration::from_secs(1),
    };
    let route_state = manager.local_pool_route_state(None);
    assert_eq!(
        route_state.kind,
        LocalPoolRouteStateKind::SchedulerRedisDegraded
    );
    assert!(route_state.retry_after_secs.is_some());

    let notifier_manager = manager.clone();
    let notifier = tokio::spawn(async move {
        loop {
            notifier_manager.notify_dispatch_state_changed();
            tokio::time::sleep(StdDuration::from_millis(1)).await;
        }
    });
    let waited_at = Instant::now();
    let queue_error = manager
        .acquire_context_for_session_with_mode(
            None,
            None,
            &HashSet::new(),
            AcquireMode::WaitForCapacityMax(StdDuration::from_millis(25)),
            1,
        )
        .await
        .err()
        .expect("Redis 退避窗口内普通本地请求应有界等待后拒绝")
        .to_string();
    assert!(
        waited_at.elapsed() >= StdDuration::from_millis(20),
        "普通本地请求不应在 Redis breaker 打开时立即形成 429 storm"
    );
    notifier.abort();
    assert!(
        queue_error.contains("Redis 调度协调状态不可用"),
        "Redis 退避不应误报为等待队列已满，实际: {queue_error}"
    );
    assert!(queue_error.contains("retry_after_secs="));
    let breaker_stats = manager.scheduler_redis_breaker.stats_snapshot();
    assert!(
        breaker_stats.suppressed <= 3,
        "Redis degraded 等待不能被普通 capacity signal 唤醒成内部重试风暴，stats={breaker_stats:?}"
    );
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
    let fail_fast_error = manager
        .acquire_context_for_session_with_mode(
            None,
            None,
            &HashSet::new(),
            AcquireMode::FailFastOnCapacity,
            1,
        )
        .await
        .err()
        .expect("external preflight fail-fast 仍应快速暴露 Redis degraded")
        .to_string();
    assert!(fail_fast_error.contains("Redis 调度协调状态不可用"));

    let bounded_wait_started = Instant::now();
    let bounded_wait_error = manager
        .acquire_context_for_session_with_mode(
            None,
            None,
            &HashSet::new(),
            AcquireMode::FailFastOnCapacityWaitForRedis(StdDuration::from_millis(25)),
            1,
        )
        .await
        .err()
        .expect("external fallback mode should wait briefly for Redis degraded recovery")
        .to_string();
    assert!(
        bounded_wait_started.elapsed() >= StdDuration::from_millis(20),
        "fallback mode must not turn Redis degraded into an immediate external storm"
    );
    assert!(
        bounded_wait_error.contains("Redis 调度协调状态不可用"),
        "bounded fallback mode should preserve the Redis degraded diagnostic"
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

    manager.scheduler_redis_breaker.state.lock().phase = SchedulerRedisBreakerPhase::Open {
        until: Instant::now() + StdDuration::from_secs(5),
    };
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn finite_redis_dispatch_queue_lease_deadline_does_not_move_after_renew_interval() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };

    let mut config = Config::default();
    config.credential_max_concurrent_requests = 1;
    config.dispatch_global_max_concurrent_requests = 1;
    config.dispatch_max_queued_requests = 1;
    config.credential_dispatch_max_wait_secs = 30;
    let manager = Arc::new(
        MultiTokenManager::new_with_stores(
            config,
            vec![api_key_credential("finite-redis-queue-no-renew")],
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
    .expect("finite waiter should enter the Redis queue");

    let initial_deadlines = redis_store
        .dispatch_queue_lease_deadlines_ms()
        .await
        .unwrap();
    assert_eq!(initial_deadlines.len(), 1);
    let initial_server_time = redis_store.server_time_ms().await.unwrap() as f64;
    let initial_remaining_ms = initial_deadlines[0].1 - initial_server_time;
    assert!(
        (85_000.0..=91_000.0).contains(&initial_remaining_ms),
        "finite queue lease must cover 30s wait plus 60s safety margin, remaining={initial_remaining_ms}ms"
    );

    tokio::time::sleep(StdDuration::from_secs(22)).await;
    assert!(!waiting.is_finished());
    let later_deadlines = redis_store
        .dispatch_queue_lease_deadlines_ms()
        .await
        .unwrap();
    assert_eq!(later_deadlines.len(), 1);
    assert_eq!(later_deadlines[0].0, initial_deadlines[0].0);
    assert_eq!(
        later_deadlines[0].1, initial_deadlines[0].1,
        "finite queue lease must not issue the old 20-second renewal"
    );

    waiting.abort();
    let join_error = match waiting.await {
        Err(error) => error,
        Ok(Ok(mut context)) => {
            context.release_in_flight();
            panic!("aborted finite queue waiter should not acquire capacity")
        }
        Ok(Err(error)) => panic!("aborted finite queue waiter returned an error: {error}"),
    };
    assert!(join_error.is_cancelled());
    first.release_in_flight();
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
    .expect("cancelled finite waiter should release its Redis queue lease");

    drop(first);
    drop(manager);
    crate::kiro::token_manager::drain_best_effort_storage_tasks(StdDuration::from_secs(1)).await;
    let cleanup = redis_store.delete_pattern_bounded("*", None).await.unwrap();
    assert!(!cleanup.cancelled);
    assert!(!cleanup.pass_limit_reached);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn redis_affinity_latency_does_not_degrade_capacity_coordination() {
    if test_redis_toxiproxy().is_none() {
        eprintln!("跳过 Redis affinity latency 测试：未设置 Toxiproxy 测试环境变量");
        return;
    }
    clear_test_redis_latency_toxic().await;
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis affinity latency 测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![api_key_credential("affinity-latency")],
        None,
        None,
        false,
        None,
        Some(redis_store),
    )
    .unwrap();

    manager.bind_session_to_credential("latency-session", 1);
    assert_eq!(manager.bound_credential_id("latency-session"), Some(1));
    set_test_redis_latency_toxic(500).await;

    let started_at = Instant::now();
    let bound_id = manager.bound_credential_id("latency-session");
    let elapsed = started_at.elapsed();
    let capacity_degraded = manager.scheduler_redis_breaker.is_degraded();
    let affinity_degraded = manager.scheduler_redis_affinity_breaker.is_degraded();

    clear_test_redis_latency_toxic().await;
    tokio::time::sleep(StdDuration::from_millis(200)).await;

    assert_eq!(bound_id, Some(1), "Redis 慢时应使用本地 sticky cache");
    assert!(
        elapsed >= SCHEDULER_REDIS_AFFINITY_OP_TIMEOUT,
        "affinity operation should exercise the 75ms timeout, elapsed={elapsed:?}"
    );
    assert!(
        !capacity_degraded,
        "affinity timeout must not open the capacity coordination breaker"
    );
    assert!(affinity_degraded);
    assert_eq!(
        manager.local_pool_route_state(None).kind,
        LocalPoolRouteStateKind::Ready
    );
    let mut context = manager
        .acquire_context(None)
        .await
        .expect("capacity coordination must recover immediately after affinity-only failure");
    context.release_in_flight();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redis_affinity_breaker_open_does_not_block_selection_admission() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis affinity/selection 隔离测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let mut config = Config::default();
    config.credential_rpm = Some(60);
    let manager = MultiTokenManager::new_with_stores(
        config,
        vec![api_key_credential("affinity-open-selection")],
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();
    manager.scheduler_redis_affinity_breaker.state.lock().phase =
        SchedulerRedisBreakerPhase::Open {
            until: Instant::now() + StdDuration::from_secs(1),
        };

    let mut context = manager
        .acquire_context_for_session_with_mode(
            None,
            Some("affinity-open-selection-session"),
            &HashSet::new(),
            AcquireMode::FailFastOnCapacityWaitForRedis(StdDuration::from_millis(50)),
            1,
        )
        .await
        .expect("sticky affinity degraded must not block capacity lease or RPM selection");
    assert_eq!(context.id, 1);
    assert!(!manager.scheduler_redis_breaker.is_degraded());
    context.release_in_flight();

    let state = redis_store
        .scheduler_state_for_credentials(&[1])
        .await
        .expect("selection state should remain readable");
    assert!(
        state
            .get(&1)
            .map(|state| state.health.recent_selection_count_60s >= 1)
            .unwrap_or(false),
        "RPM/selection accounting must use the scheduler hot path, not the open affinity breaker"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn redis_capacity_latency_boundary_and_recovery_matrix() {
    if test_redis_toxiproxy().is_none() {
        eprintln!("跳过 Redis capacity latency 矩阵：未设置 Toxiproxy 测试环境变量");
        return;
    }

    for latency_ms in [50_u64, 500] {
        for round in 1..=3 {
            clear_test_redis_latency_toxic().await;
            let Some(redis_store) = test_redis_store().await else {
                eprintln!("跳过 Redis capacity latency 矩阵：未设置 KIRO_RS_TEST_REDIS_URL");
                return;
            };
            let manager = MultiTokenManager::new_with_stores(
                Config::default(),
                vec![api_key_credential(&format!(
                    "capacity-{latency_ms}-{round}"
                ))],
                None,
                None,
                false,
                None,
                Some(redis_store),
            )
            .unwrap();
            set_test_redis_latency_toxic(latency_ms).await;

            let started_at = Instant::now();
            let result = manager.acquire_in_flight_slot(1, 1).await;
            let elapsed = started_at.elapsed();
            let succeeded = matches!(result, Ok(Some(_)));
            let error_message = result.as_ref().err().map(ToString::to_string);
            let degraded = manager.scheduler_redis_breaker.is_degraded();

            clear_test_redis_latency_toxic().await;
            tokio::time::sleep(StdDuration::from_millis(latency_ms.saturating_add(50))).await;

            assert!(
                elapsed < SCHEDULER_REDIS_HOT_OP_TIMEOUT + StdDuration::from_millis(175),
                "hot-path timeout must remain bounded at {latency_ms}ms round {round}: {elapsed:?}"
            );
            if latency_ms == 50 {
                assert!(
                    succeeded,
                    "50ms response latency should remain below the capacity hot-path budget: {error_message:?}"
                );
                assert!(
                    !degraded,
                    "50ms response latency must not degrade capacity coordination"
                );
            }
            if latency_ms >= 500 {
                assert!(
                    !succeeded,
                    "{latency_ms}ms response latency must fail closed at round {round}"
                );
                assert!(
                    !degraded,
                    "a single timeout should fail this request closed without immediately opening the capacity breaker"
                );
            }

            eprintln!(
                "redis-capacity-latency latency_ms={latency_ms} round={round} succeeded={succeeded} elapsed_ms={} degraded={degraded}",
                elapsed.as_millis()
            );

            if let Ok(Some(lease)) = result {
                drop(lease);
            } else {
                assert_ne!(
                    manager.local_pool_route_state(None).kind,
                    LocalPoolRouteStateKind::AllDisabled,
                    "a capacity timeout must not be reported as all credentials disabled"
                );
                tokio::time::sleep(StdDuration::from_millis(latency_ms.saturating_add(150))).await;
            }

            let recovered = manager
                .acquire_in_flight_slot(1, 1)
                .await
                .expect("Redis coordination should accept requests after latency removal/backoff")
                .expect("the only credential should have capacity after recovery");
            drop(recovered);
            assert!(!manager.scheduler_redis_breaker.is_degraded());
        }
    }
    clear_test_redis_latency_toxic().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn redis_capacity_consecutive_timeouts_open_breaker_without_all_disabled() {
    if test_redis_toxiproxy().is_none() {
        eprintln!("跳过 Redis capacity timeout breaker 测试：未设置 Toxiproxy 测试环境变量");
        return;
    }
    clear_test_redis_latency_toxic().await;
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis capacity timeout breaker 测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![api_key_credential("capacity-timeout-breaker")],
        None,
        None,
        false,
        None,
        Some(redis_store),
    )
    .unwrap();
    set_test_redis_latency_toxic(500).await;

    for attempt in 1..SCHEDULER_REDIS_TIMEOUT_FAILURES_TO_OPEN {
        let result = manager.acquire_in_flight_slot(1, 1).await;
        assert!(
            result.is_err(),
            "slow Redis attempt {attempt} must fail this request closed"
        );
        assert!(
            !manager.scheduler_redis_breaker.is_degraded(),
            "slow Redis attempt {attempt} must not open breaker before the consecutive threshold"
        );
        assert_ne!(
            manager.local_pool_route_state(None).kind,
            LocalPoolRouteStateKind::AllDisabled,
            "slow Redis attempt {attempt} must not be reported as all credentials disabled"
        );
    }

    let threshold_result = manager.acquire_in_flight_slot(1, 1).await;
    assert!(
        threshold_result.is_err(),
        "threshold slow Redis attempt must fail this request closed"
    );
    assert!(
        manager.scheduler_redis_breaker.is_degraded(),
        "capacity breaker must open after repeated consecutive timeout failures"
    );
    assert_eq!(
        manager.local_pool_route_state(None).kind,
        LocalPoolRouteStateKind::SchedulerRedisDegraded
    );

    clear_test_redis_latency_toxic().await;
    let retry_after = manager
        .scheduler_redis_breaker
        .retry_after()
        .unwrap_or(SCHEDULER_REDIS_DEGRADED_BACKOFF_BASE);
    tokio::time::sleep(retry_after + StdDuration::from_millis(100)).await;
    let recovered = manager
        .acquire_in_flight_slot(1, 1)
        .await
        .expect("Redis coordination should recover after latency removal/backoff")
        .expect("the only credential should recover capacity");
    drop(recovered);
    assert!(!manager.scheduler_redis_breaker.is_degraded());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn redis_usage_writer_and_scheduler_joint_fault_matrix_recovers_without_spin_or_false_disable()
 {
    if test_redis_toxiproxy().is_none() {
        eprintln!("跳过 Redis usage/scheduler 联合故障矩阵：未设置 Toxiproxy 测试环境变量");
        return;
    }

    run_isolated_redis_manager_fixture(|redis_store| async move {
        const WRITERS: usize = 4;
        const LOW_LATENCY_RECORDS_PER_WRITER: usize = 4;
        const FAULT_RECORDS_PER_WRITER: usize = 2;
        const LOW_LATENCIES_MS: [u64; 6] = [25, 50, 74, 75, 90, 150];

        clear_test_redis_latency_toxic().await;
        set_test_redis_proxy_enabled(true).await;
        let rss_start = process_rss_kib_for_test();
        let fd_start = open_fd_count_for_test();

        for round in 1..=3 {
            for latency_ms in LOW_LATENCIES_MS {
                let scenario = format!("latency-{latency_ms}-round-{round}");
                let manager = MultiTokenManager::new_with_stores(
                    Config::default(),
                    vec![api_key_credential(&scenario)],
                    None,
                    None,
                    false,
                    None,
                    Some(redis_store.clone()),
                )
                .unwrap();
                redis_store.reset_usage_summary_write_round_trips();
                set_test_redis_latency_toxic(latency_ms).await;
                let start = Arc::new(tokio::sync::Barrier::new(WRITERS + 1));
                let usage = spawn_usage_fault_wave(
                    redis_store.clone(),
                    scenario.clone(),
                    WRITERS,
                    LOW_LATENCY_RECORDS_PER_WRITER,
                    start.clone(),
                );
                start.wait().await;

                let scheduler_started = Instant::now();
                let scheduler_result = manager.acquire_in_flight_slot(1, 1).await;
                let scheduler_elapsed = scheduler_started.elapsed();
                let scheduler_lease = scheduler_result
                    .unwrap_or_else(|error| {
                        let breaker = manager.scheduler_redis_breaker.stats_snapshot();
                        panic!(
                            "{scenario}: scheduler must remain available below the 250ms capacity deadline: \
                             error={error}; elapsed={scheduler_elapsed:?}; \
                             breaker_degraded={}; breaker_admitted={}; breaker_failures={}; \
                             breaker_fail_fast={}; usage_round_trips={}; route_state={:?}",
                            manager.scheduler_redis_breaker.is_degraded(),
                            breaker.admitted,
                            breaker.failures,
                            breaker.fail_fast,
                            redis_store.usage_summary_write_round_trips(),
                            manager.local_pool_route_state(None).kind,
                        )
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "{scenario}: scheduler unexpectedly had no capacity; \
                             elapsed={scheduler_elapsed:?}; usage_round_trips={}; route_state={:?}",
                            redis_store.usage_summary_write_round_trips(),
                            manager.local_pool_route_state(None).kind,
                        )
                    });
                drop(scheduler_lease);
                let usage = tokio::time::timeout(StdDuration::from_secs(8), usage)
                    .await
                    .unwrap_or_else(|_| panic!("{scenario}: usage writer wave did not finish"))
                    .unwrap();
                clear_test_redis_latency_toxic().await;

                let expected_records = (WRITERS * LOW_LATENCY_RECORDS_PER_WRITER) as u64;
                assert_eq!(usage.attempted, expected_records, "{scenario}");
                assert_eq!(usage.succeeded, expected_records, "{scenario}");
                assert_eq!(usage.failed, 0, "{scenario}");
                assert_eq!(usage.timed_out, 0, "{scenario}");
                assert_eq!(
                    redis_store.usage_summary_write_round_trips(),
                    expected_records,
                    "{scenario}: usage writes must remain one Redis RTT each without retries"
                );
                assert!(
                    scheduler_elapsed
                        < SCHEDULER_REDIS_HOT_OP_TIMEOUT + StdDuration::from_millis(175),
                    "{scenario}: scheduler exceeded its bounded hot-path deadline: {scheduler_elapsed:?}"
                );
                assert!(
                    !manager.scheduler_redis_breaker.is_degraded(),
                    "{scenario}: sub-deadline latency must not open the capacity breaker"
                );
                assert_ne!(
                    manager.local_pool_route_state(None).kind,
                    LocalPoolRouteStateKind::AllDisabled,
                    "{scenario}: Redis latency must never impersonate credential disablement"
                );
                crate::kiro::token_manager::drain_best_effort_storage_tasks(
                    StdDuration::from_secs(3),
                )
                .await;
                eprintln!(
                    "redis-joint-fault scenario={scenario} usage_attempts={} usage_success={} usage_round_trips={} scheduler_elapsed_ms={} breaker_degraded=false",
                    usage.attempted,
                    usage.succeeded,
                    redis_store.usage_summary_write_round_trips(),
                    scheduler_elapsed.as_millis(),
                );
            }

            let scenario = format!("latency-500-round-{round}");
            let manager = MultiTokenManager::new_with_stores(
                Config::default(),
                vec![api_key_credential(&scenario)],
                None,
                None,
                false,
                None,
                Some(redis_store.clone()),
            )
            .unwrap();
            redis_store.reset_usage_summary_write_round_trips();
            set_test_redis_latency_toxic(500).await;
            let start = Arc::new(tokio::sync::Barrier::new(WRITERS + 1));
            let usage = spawn_usage_fault_wave(
                redis_store.clone(),
                scenario.clone(),
                WRITERS,
                FAULT_RECORDS_PER_WRITER,
                start.clone(),
            );
            start.wait().await;
            let stats_before = manager.scheduler_redis_breaker.stats_snapshot();
            for attempt in 1..=SCHEDULER_REDIS_TIMEOUT_FAILURES_TO_OPEN {
                let started = Instant::now();
                assert!(
                    manager.acquire_in_flight_slot(1, 1).await.is_err(),
                    "{scenario}: attempt {attempt} must fail closed"
                );
                assert!(
                    started.elapsed()
                        < SCHEDULER_REDIS_HOT_OP_TIMEOUT + StdDuration::from_millis(175),
                    "{scenario}: attempt {attempt} exceeded the hot-path deadline"
                );
                assert_ne!(
                    manager.local_pool_route_state(None).kind,
                    LocalPoolRouteStateKind::AllDisabled,
                    "{scenario}: attempt {attempt} must not report AllDisabled"
                );
            }
            let stats_open = manager.scheduler_redis_breaker.stats_snapshot();
            assert!(manager.scheduler_redis_breaker.is_degraded(), "{scenario}");
            assert_eq!(
                stats_open
                    .failures
                    .saturating_sub(stats_before.failures),
                u64::from(SCHEDULER_REDIS_TIMEOUT_FAILURES_TO_OPEN),
                "{scenario}: exactly the configured number of Redis operations may fail before opening"
            );
            assert_eq!(
                stats_open.admitted.saturating_sub(stats_before.admitted),
                u64::from(SCHEDULER_REDIS_TIMEOUT_FAILURES_TO_OPEN),
                "{scenario}: scheduler attempts must not be internally amplified"
            );
            assert_capacity_breaker_fail_fast_without_redis_spin(&manager, &scenario).await;
            clear_test_redis_latency_toxic().await;
            let usage = tokio::time::timeout(StdDuration::from_secs(8), usage)
                .await
                .unwrap_or_else(|_| panic!("{scenario}: usage writer wave did not finish"))
                .unwrap();
            assert_eq!(
                usage.attempted,
                (WRITERS * FAULT_RECORDS_PER_WRITER) as u64,
                "{scenario}"
            );
            assert_eq!(
                redis_store.usage_summary_write_round_trips(),
                usage.attempted,
                "{scenario}: usage failures/successes must not retry internally"
            );
            recover_capacity_breaker_five_times(&manager, &redis_store, &scenario).await;
            let stats_recovered = manager.scheduler_redis_breaker.stats_snapshot();
            eprintln!(
                "redis-joint-fault scenario={scenario} usage_attempts={} usage_success={} usage_failed={} usage_timed_out={} usage_round_trips={} redis_admitted_delta={} redis_failures_delta={} fail_fast_delta={} recovered=true",
                usage.attempted,
                usage.succeeded,
                usage.failed,
                usage.timed_out,
                redis_store.usage_summary_write_round_trips(),
                stats_recovered.admitted.saturating_sub(stats_before.admitted),
                stats_recovered.failures.saturating_sub(stats_before.failures),
                stats_recovered.fail_fast.saturating_sub(stats_before.fail_fast),
            );

            let scenario = format!("wrongtype-round-{round}");
            let manager = MultiTokenManager::new_with_stores(
                Config::default(),
                vec![api_key_credential(&scenario)],
                None,
                None,
                false,
                None,
                Some(redis_store.clone()),
            )
            .unwrap();
            redis_store.reset_usage_summary_write_round_trips();
            redis_store
                .set_raw_string_for_test(
                    "scheduler:inflight:1:last_seen",
                    "deliberate-wrongtype",
                )
                .await
                .unwrap();
            let start = Arc::new(tokio::sync::Barrier::new(WRITERS + 1));
            let usage = spawn_usage_fault_wave(
                redis_store.clone(),
                scenario.clone(),
                WRITERS,
                LOW_LATENCY_RECORDS_PER_WRITER,
                start.clone(),
            );
            start.wait().await;
            assert!(
                manager.acquire_in_flight_slot(1, 1).await.is_err(),
                "{scenario}: scheduler WRONGTYPE must fail closed"
            );
            assert!(manager.scheduler_redis_breaker.is_degraded(), "{scenario}");
            assert_ne!(
                manager.local_pool_route_state(None).kind,
                LocalPoolRouteStateKind::AllDisabled,
                "{scenario}: Redis protocol/type errors must not disable the credential"
            );
            assert_capacity_breaker_fail_fast_without_redis_spin(&manager, &scenario).await;
            redis_store
                .del("scheduler:inflight:1:last_seen")
                .await
                .unwrap();
            let usage = tokio::time::timeout(StdDuration::from_secs(8), usage)
                .await
                .unwrap_or_else(|_| panic!("{scenario}: usage writer wave did not finish"))
                .unwrap();
            assert_eq!(usage.succeeded, usage.attempted, "{scenario}");
            assert_eq!(
                redis_store.usage_summary_write_round_trips(),
                usage.attempted,
                "{scenario}: usage writer must remain one-RTT and independent of scheduler key type"
            );
            recover_capacity_breaker_five_times(&manager, &redis_store, &scenario).await;
            let wrongtype_stats = manager.scheduler_redis_breaker.stats_snapshot();
            eprintln!(
                "redis-joint-fault scenario={scenario} usage_attempts={} usage_success={} usage_failed={} usage_timed_out={} usage_round_trips={} redis_admitted={} redis_failures={} fail_fast={} recovered=true",
                usage.attempted,
                usage.succeeded,
                usage.failed,
                usage.timed_out,
                redis_store.usage_summary_write_round_trips(),
                wrongtype_stats.admitted,
                wrongtype_stats.failures,
                wrongtype_stats.fail_fast,
            );

            let scenario = format!("disconnect-round-{round}");
            let manager = MultiTokenManager::new_with_stores(
                Config::default(),
                vec![api_key_credential(&scenario)],
                None,
                None,
                false,
                None,
                Some(redis_store.clone()),
            )
            .unwrap();
            redis_store.reset_usage_summary_write_round_trips();
            let healthy = manager
                .acquire_in_flight_slot(1, 1)
                .await
                .unwrap()
                .unwrap();
            drop(healthy);
            crate::kiro::token_manager::drain_best_effort_storage_tasks(
                StdDuration::from_secs(2),
            )
            .await;
            let start = Arc::new(tokio::sync::Barrier::new(WRITERS + 1));
            let usage = spawn_usage_fault_wave(
                redis_store.clone(),
                scenario.clone(),
                WRITERS,
                FAULT_RECORDS_PER_WRITER,
                start.clone(),
            );
            set_test_redis_latency_toxic(500).await;
            start.wait().await;
            tokio::time::timeout(StdDuration::from_secs(2), async {
                while redis_store.usage_summary_write_round_trips() == 0 {
                    tokio::time::sleep(StdDuration::from_millis(2)).await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("{scenario}: usage writer did not enter Redis"));
            set_test_redis_proxy_enabled(false).await;
            let mut attempts = 0_u32;
            while !manager.scheduler_redis_breaker.is_degraded()
                && attempts < SCHEDULER_REDIS_TIMEOUT_FAILURES_TO_OPEN
            {
                attempts += 1;
                assert!(
                    manager.acquire_in_flight_slot(1, 1).await.is_err(),
                    "{scenario}: disconnected scheduler attempt {attempts} must fail closed"
                );
                assert_ne!(
                    manager.local_pool_route_state(None).kind,
                    LocalPoolRouteStateKind::AllDisabled,
                    "{scenario}: disconnect must not report AllDisabled"
                );
            }
            assert!(
                manager.scheduler_redis_breaker.is_degraded(),
                "{scenario}: disconnect must open the breaker within the bounded threshold"
            );
            assert!(
                attempts <= SCHEDULER_REDIS_TIMEOUT_FAILURES_TO_OPEN,
                "{scenario}: disconnect attempts were internally amplified"
            );
            assert_capacity_breaker_fail_fast_without_redis_spin(&manager, &scenario).await;
            set_test_redis_proxy_enabled(true).await;
            clear_test_redis_latency_toxic().await;
            let usage = tokio::time::timeout(StdDuration::from_secs(8), usage)
                .await
                .unwrap_or_else(|_| panic!("{scenario}: usage writer wave did not finish"))
                .unwrap();
            assert_eq!(
                usage.attempted,
                (WRITERS * FAULT_RECORDS_PER_WRITER) as u64,
                "{scenario}"
            );
            assert_eq!(
                redis_store.usage_summary_write_round_trips(),
                usage.attempted,
                "{scenario}: reconnect must not cause hidden usage retries"
            );
            recover_capacity_breaker_five_times(&manager, &redis_store, &scenario).await;
            let disconnect_stats = manager.scheduler_redis_breaker.stats_snapshot();
            eprintln!(
                "redis-joint-fault scenario={scenario} scheduler_attempts={attempts} usage_attempts={} usage_success={} usage_failed={} usage_timed_out={} usage_round_trips={} redis_admitted={} redis_failures={} fail_fast={} recovered=true",
                usage.attempted,
                usage.succeeded,
                usage.failed,
                usage.timed_out,
                redis_store.usage_summary_write_round_trips(),
                disconnect_stats.admitted,
                disconnect_stats.failures,
                disconnect_stats.fail_fast,
            );

            assert_eq!(
                redis_store.global_capacity_state().await.unwrap().queued_requests,
                0,
                "round {round}: joint faults must not leave dispatch queue entries"
            );
        }

        clear_test_redis_latency_toxic().await;
        set_test_redis_proxy_enabled(true).await;
        crate::kiro::token_manager::drain_best_effort_storage_tasks(StdDuration::from_secs(5)).await;
        let rss_end = process_rss_kib_for_test();
        let fd_end = open_fd_count_for_test();
        if let (Some(start), Some(end)) = (rss_start, rss_end) {
            assert!(
                end <= start.saturating_add(32 * 1024),
                "joint fault RSS did not recover within 32 MiB: start={start}, end={end}"
            );
        }
        if let (Some(start), Some(end)) = (fd_start, fd_end) {
            assert!(
                end <= start.saturating_add(8),
                "joint fault test leaked file descriptors: start={start}, end={end}"
            );
        }
        eprintln!(
            "redis-joint-fault resources rss_start_kib={rss_start:?} rss_end_kib={rss_end:?} fd_start={fd_start:?} fd_end={fd_end:?}"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn redis_business_and_observability_fault_domains_are_independent_for_three_rounds() {
    let Some(business_url) = std::env::var("KIRO_RS_TEST_BUSINESS_REDIS_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        eprintln!("跳过双 Redis 故障域测试：未设置 KIRO_RS_TEST_BUSINESS_REDIS_URL");
        return;
    };
    let Some(observability_url) = std::env::var("KIRO_RS_TEST_OBSERVABILITY_REDIS_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        eprintln!("跳过双 Redis 故障域测试：未设置 KIRO_RS_TEST_OBSERVABILITY_REDIS_URL");
        return;
    };
    let Some(_) = test_fault_domain_toxiproxy("business") else {
        eprintln!("跳过双 Redis 故障域测试：未设置 business Toxiproxy variables");
        return;
    };
    let Some(_) = test_fault_domain_toxiproxy("observability") else {
        eprintln!("跳过双 Redis 故障域测试：未设置 observability Toxiproxy variables");
        return;
    };

    let mut business_config = Config::default();
    business_config.redis.url = Some(business_url);
    business_config.redis.key_prefix = format!(
        "kiro_rs:test:business-fault-domain:{}",
        uuid::Uuid::new_v4()
    );
    let business_store = Arc::new(RedisStore::connect(&business_config).await.unwrap());

    let observability_config = crate::model::config::RedisConfig {
        url: Some(observability_url),
        key_prefix: format!(
            "kiro_rs:test:observability-fault-domain:{}",
            uuid::Uuid::new_v4()
        ),
    };
    let observability_store = Arc::new(
        RedisStore::connect_observability(&observability_config)
            .await
            .unwrap(),
    );
    assert!(!business_store.is_observability());
    assert!(observability_store.is_observability());
    assert_ne!(
        business_store.server_run_id().await.unwrap(),
        observability_store.server_run_id().await.unwrap(),
        "the two configured authorities must be different Redis server processes"
    );

    let outcome = AssertUnwindSafe(async {
        set_fault_domain_proxy_enabled("business", true).await;
        set_fault_domain_proxy_enabled("observability", true).await;
        clear_fault_domain_latency("business").await;
        clear_fault_domain_latency("observability").await;
        wait_fault_domain_store_healthy(&business_store, "business").await;
        wait_fault_domain_store_healthy(&observability_store, "observability").await;

        for round in 1..=3 {
            let manager = MultiTokenManager::new_with_stores(
                Config::default(),
                vec![api_key_credential(&format!("redis-fault-domain-{round}"))],
                None,
                None,
                false,
                None,
                Some(business_store.clone()),
            )
            .unwrap();

            let warm = manager
                .acquire_in_flight_slot(1, 1)
                .await
                .unwrap()
                .expect("business Redis warm-up must have capacity");
            drop(warm);
            crate::kiro::token_manager::drain_best_effort_storage_tasks(
                StdDuration::from_secs(2),
            )
            .await;
            let baseline_stats = manager.scheduler_redis_breaker.stats_snapshot();

            for latency_ms in [50_u64, 150, 500] {
                set_fault_domain_latency("observability", latency_ms).await;
                let started = Instant::now();
                let lease = tokio::time::timeout(
                    StdDuration::from_millis(800),
                    manager.acquire_in_flight_slot(1, 1),
                )
                .await
                .unwrap_or_else(|_| {
                    panic!(
                        "round {round} observability latency {latency_ms}ms blocked business scheduler"
                    )
                })
                .unwrap()
                .expect("business scheduler must remain available while observability is slow");
                assert!(
                    started.elapsed() < StdDuration::from_millis(500),
                    "round {round} observability latency {latency_ms}ms inflated business scheduler"
                );
                drop(lease);

                let record = sampled_request_rejection_usage_record(
                    &format!("redis-fault-domain-{round}-{latency_ms}"),
                    "/cc/v1/messages",
                    None,
                    "redis_fault_domain_validation",
                    "observability_writer",
                    http::StatusCode::SERVICE_UNAVAILABLE,
                    latency_ms,
                );
                let usage = tokio::time::timeout(
                    StdDuration::from_secs(8),
                    observability_store.record_usage_summary(&record),
                )
                .await
                .unwrap_or_else(|_| {
                    panic!(
                        "round {round} observability usage write timed out at {latency_ms}ms"
                    )
                })
                .unwrap();
                assert!(usage, "round {round} observability usage write was not committed");
                let stats = manager.scheduler_redis_breaker.stats_snapshot();
                assert_eq!(stats.failures, baseline_stats.failures);
                clear_fault_domain_latency("observability").await;
            }

            set_fault_domain_proxy_enabled("observability", false).await;
            let before_disconnect = manager.scheduler_redis_breaker.stats_snapshot();
            for index in 0..5 {
                let started = Instant::now();
                let lease = tokio::time::timeout(
                    StdDuration::from_millis(800),
                    manager.acquire_in_flight_slot(1, 1),
                )
                .await
                .unwrap_or_else(|_| panic!("round {round} observation disconnect blocked scheduler"))
                .unwrap_or_else(|error| panic!("round {round} scheduler recovery probe {index} failed: {error}"))
                .expect("business scheduler lost capacity during observation disconnect");
                assert!(started.elapsed() < StdDuration::from_millis(500));
                drop(lease);
            }
            let after_disconnect = manager.scheduler_redis_breaker.stats_snapshot();
            assert_eq!(after_disconnect.failures, before_disconnect.failures);
            set_fault_domain_proxy_enabled("observability", true).await;
            clear_fault_domain_latency("observability").await;
            wait_fault_domain_store_healthy(&observability_store, "observability").await;
            for index in 0..5 {
                let record = sampled_request_rejection_usage_record(
                    &format!("redis-fault-domain-recovery-{round}-{index}"),
                    "/cc/v1/messages",
                    None,
                    "redis_fault_domain_validation",
                    "observability_recovery",
                    http::StatusCode::SERVICE_UNAVAILABLE,
                    index,
                );
                assert!(
                    observability_store.record_usage_summary(&record).await.unwrap(),
                    "round {round} observation recovery write {index} failed"
                );
            }

            let fault_manager = MultiTokenManager::new_with_stores(
                Config::default(),
                vec![api_key_credential(&format!("redis-business-fault-{round}"))],
                None,
                None,
                false,
                None,
                Some(business_store.clone()),
            )
            .unwrap();
            set_fault_domain_proxy_enabled("business", false).await;
            let failed = tokio::time::timeout(
                StdDuration::from_millis(800),
                fault_manager.acquire_in_flight_slot(1, 1),
            )
            .await
            .unwrap_or_else(|_| panic!("round {round} business Redis fault was not bounded"));
            assert!(
                !matches!(failed, Ok(Some(_))),
                "round {round} business Redis fault must never report a successful lease"
            );
            assert_ne!(
                fault_manager.local_pool_route_state(None).kind,
                LocalPoolRouteStateKind::AllDisabled,
                "round {round} business Redis fault must not impersonate credential disablement"
            );
            let observer_record = sampled_request_rejection_usage_record(
                &format!("redis-business-fault-observer-{round}"),
                "/cc/v1/messages",
                None,
                "redis_fault_domain_validation",
                "observability_business_fault",
                http::StatusCode::SERVICE_UNAVAILABLE,
                round as u64,
            );
            assert!(
                observability_store
                    .record_usage_summary(&observer_record)
                    .await
                    .unwrap(),
                "round {round} observability write must survive business Redis fault"
            );
            set_fault_domain_proxy_enabled("business", true).await;
            clear_fault_domain_latency("business").await;
            wait_fault_domain_store_healthy(&business_store, "business").await;
            let recovered_manager = MultiTokenManager::new_with_stores(
                Config::default(),
                vec![api_key_credential(&format!("redis-business-recovery-{round}"))],
                None,
                None,
                false,
                None,
                Some(business_store.clone()),
            )
            .unwrap();
            recover_capacity_breaker_five_times(
                &recovered_manager,
                &business_store,
                &format!("round {round} business Redis fault recovery"),
            )
            .await;
        }
    })
    .catch_unwind()
    .await;

    let _ = AssertUnwindSafe(async {
        set_fault_domain_proxy_enabled("business", true).await;
        set_fault_domain_proxy_enabled("observability", true).await;
        clear_fault_domain_latency("business").await;
        clear_fault_domain_latency("observability").await;
    })
    .catch_unwind()
    .await;
    crate::kiro::token_manager::drain_best_effort_storage_tasks(StdDuration::from_secs(5)).await;
    business_store
        .delete_pattern_bounded("*", None)
        .await
        .unwrap();
    observability_store
        .delete_pattern_bounded("*", None)
        .await
        .unwrap();
    if let Err(payload) = outcome {
        std::panic::resume_unwind(payload);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn redis_lease_release_is_non_blocking_under_latency_and_burst() {
    if test_redis_toxiproxy().is_none() {
        eprintln!("跳过 Redis lease release latency 测试：未设置 Toxiproxy 测试环境变量");
        return;
    }
    clear_test_redis_latency_toxic().await;
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis lease release latency 测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let mut test_config = Config::default();
    // This test intentionally creates 300 simultaneous leases to verify that
    // dropping the guards never waits on Redis. Keep the fixture capacity
    // unlimited; the production default remains bounded.
    test_config.credential_max_concurrent_requests = 0;
    let manager = MultiTokenManager::new_with_stores(
        test_config,
        vec![api_key_credential("release-latency-burst")],
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();

    const LEASES: usize = 300;
    let mut leases = Vec::with_capacity(LEASES);
    for _ in 0..LEASES {
        leases.push(
            manager
                .acquire_in_flight_slot(1, 1)
                .await
                .expect("Redis lease acquire")
                .expect("unlimited test credential capacity"),
        );
    }
    assert_eq!(manager.entries.lock()[0].in_flight_requests, LEASES as u32);

    let dispatcher = manager
        .scheduler_redis_release_dispatcher
        .as_ref()
        .expect("Redis manager release dispatcher");
    let stats_before = dispatcher.snapshot();
    set_test_redis_latency_toxic(500).await;
    let started_at = Instant::now();
    drop(leases);
    let release_call_elapsed = started_at.elapsed();
    tokio::time::sleep(StdDuration::from_millis(100)).await;
    clear_test_redis_latency_toxic().await;

    assert!(
        release_call_elapsed < StdDuration::from_millis(100),
        "dropping {LEASES} leases must not synchronously wait for Redis: {release_call_elapsed:?}"
    );
    assert_eq!(manager.entries.lock()[0].in_flight_requests, 0);

    assert!(
        manager
            .drain_scheduler_redis_releases(StdDuration::from_secs(10))
            .await,
        "Redis release reconciliation must drain"
    );
    let stats_after = dispatcher.snapshot();
    assert!(
        stats_after.enqueued.saturating_sub(stats_before.enqueued) >= LEASES as u64,
        "every release must enter a bounded background lane"
    );
    assert_eq!(stats_after.pending, 0);
    assert_eq!(
        stats_after.capacity_available,
        stats_before.capacity_available + LEASES
    );

    tokio::time::timeout(StdDuration::from_secs(5), async {
        loop {
            let state = redis_store
                .scheduler_state_for_credentials(&[1])
                .await
                .expect("read Redis scheduler state after reconciliation");
            if state
                .get(&1)
                .is_none_or(|state| state.in_flight_leases.is_empty())
            {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(25)).await;
        }
    })
    .await
    .expect("all Redis leases should be reconciled after latency removal");

    eprintln!(
        "redis-lease-release burst={LEASES} drop_elapsed_ms={} retries_delta={} enqueued_delta={}",
        release_call_elapsed.as_millis(),
        stats_after.retries.saturating_sub(stats_before.retries),
        stats_after.enqueued.saturating_sub(stats_before.enqueued),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn redis_capacity_disconnect_reconnect_recovers_same_manager() {
    if test_redis_toxiproxy().is_none() {
        eprintln!("跳过 Redis reconnect 测试：未设置 Toxiproxy 测试环境变量");
        return;
    }
    clear_test_redis_latency_toxic().await;
    set_test_redis_proxy_enabled(true).await;

    for round in 1..=3 {
        let Some(redis_store) = test_redis_store().await else {
            eprintln!("跳过 Redis reconnect 测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };
        let manager = MultiTokenManager::new_with_stores(
            Config::default(),
            vec![api_key_credential(&format!("disconnect-recovery-{round}"))],
            None,
            None,
            false,
            None,
            Some(redis_store.clone()),
        )
        .unwrap();
        let healthy = manager
            .acquire_in_flight_slot(1, 1)
            .await
            .expect("healthy Redis acquire")
            .expect("healthy credential capacity");
        drop(healthy);
        crate::kiro::token_manager::drain_best_effort_storage_tasks(StdDuration::from_secs(2))
            .await;

        set_test_redis_proxy_enabled(false).await;
        let started_at = Instant::now();
        let disconnected = manager.acquire_in_flight_slot(1, 1).await;
        let elapsed = started_at.elapsed();
        let first_error = disconnected.as_ref().err().map(ToString::to_string);
        let mut attempts = 1_u32;
        while !manager.scheduler_redis_breaker.is_degraded()
            && attempts < SCHEDULER_REDIS_TIMEOUT_FAILURES_TO_OPEN
        {
            attempts += 1;
            let repeated = manager.acquire_in_flight_slot(1, 1).await;
            assert!(
                repeated.is_err(),
                "disconnect round {round} attempt {attempts} must fail closed"
            );
            assert_ne!(
                manager.local_pool_route_state(None).kind,
                LocalPoolRouteStateKind::AllDisabled,
                "disconnect round {round} attempt {attempts} must not be reported as all disabled"
            );
        }
        let degraded = manager.scheduler_redis_breaker.is_degraded();
        set_test_redis_proxy_enabled(true).await;

        assert!(
            disconnected.is_err(),
            "disconnect round {round} must fail closed"
        );
        assert!(
            degraded,
            "disconnect round {round} must open coordination breaker within {attempts} attempts; first_error={first_error:?}"
        );
        assert!(
            elapsed < SCHEDULER_REDIS_HOT_OP_TIMEOUT + StdDuration::from_millis(175),
            "disconnect round {round} must respect hot-path deadline: {elapsed:?}"
        );
        assert_eq!(
            manager.local_pool_route_state(None).kind,
            LocalPoolRouteStateKind::SchedulerRedisDegraded
        );

        let retry_after = manager
            .scheduler_redis_breaker
            .retry_after()
            .unwrap_or(SCHEDULER_REDIS_DEGRADED_BACKOFF_BASE);
        tokio::time::sleep(retry_after + StdDuration::from_millis(100)).await;
        let recovered = manager
            .acquire_in_flight_slot(1, 1)
            .await
            .expect("same manager should reconnect after Redis proxy recovery")
            .expect("credential capacity should recover");
        drop(recovered);
        crate::kiro::token_manager::drain_best_effort_storage_tasks(StdDuration::from_secs(2))
            .await;
        assert!(!manager.scheduler_redis_breaker.is_degraded());
        tokio::time::timeout(StdDuration::from_secs(2), async {
            loop {
                match redis_store.scheduler_state_for_credentials(&[1]).await {
                    Ok(state)
                        if state
                            .get(&1)
                            .is_none_or(|state| state.in_flight_leases.is_empty()) =>
                    {
                        break;
                    }
                    Ok(_) | Err(_) => {
                        tokio::time::sleep(StdDuration::from_millis(25)).await;
                    }
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("reconnect round {round} must clear Redis leases"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn redis_capacity_backend_restart_recovers_same_manager() {
    let Ok(container) = std::env::var("KIRO_RS_TEST_REDIS_RESTART_CONTAINER") else {
        eprintln!("跳过 Redis backend restart 测试：未设置专用测试容器");
        return;
    };
    assert!(
        container.starts_with("kiro-rs-validation-redis-"),
        "只允许重启本任务创建的 Redis validation 容器"
    );
    if test_redis_toxiproxy().is_none() {
        eprintln!("跳过 Redis backend restart 测试：未设置 Toxiproxy 测试环境变量");
        return;
    }
    clear_test_redis_latency_toxic().await;
    set_test_redis_proxy_enabled(true).await;
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis backend restart 测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![api_key_credential("backend-restart")],
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();

    for round in 1..=3 {
        let stop_status = std::process::Command::new("docker")
            .args(["stop", "--timeout", "0", &container])
            .status()
            .expect("run docker stop for validation Redis");
        assert!(stop_status.success(), "stop validation Redis round {round}");

        let started_at = Instant::now();
        let unavailable = manager.acquire_in_flight_slot(1, 1).await;
        let elapsed = started_at.elapsed();
        let degraded = manager.scheduler_redis_breaker.is_degraded();

        let start_status = std::process::Command::new("docker")
            .args(["start", &container])
            .status()
            .expect("run docker start for validation Redis");
        assert!(
            start_status.success(),
            "start validation Redis round {round}"
        );

        assert!(
            unavailable.is_err(),
            "stopped Redis round {round} must fail closed"
        );
        assert!(
            degraded,
            "stopped Redis round {round} must open coordination breaker"
        );
        assert!(
            elapsed < SCHEDULER_REDIS_HOT_OP_TIMEOUT + StdDuration::from_millis(175),
            "stopped Redis round {round} must respect hot deadline: {elapsed:?}"
        );

        tokio::time::sleep(SCHEDULER_REDIS_DEGRADED_BACKOFF_BASE).await;
        tokio::time::sleep(StdDuration::from_millis(200)).await;
        let recovered = manager
            .acquire_in_flight_slot(1, 1)
            .await
            .expect("same manager should reconnect after Redis backend restart")
            .expect("credential capacity should recover after backend restart");
        drop(recovered);
        crate::kiro::token_manager::drain_best_effort_storage_tasks(StdDuration::from_secs(2))
            .await;
        assert!(!manager.scheduler_redis_breaker.is_degraded());

        tokio::time::timeout(StdDuration::from_secs(2), async {
            loop {
                match redis_store.scheduler_state_for_credentials(&[1]).await {
                    Ok(state)
                        if state
                            .get(&1)
                            .is_none_or(|state| state.in_flight_leases.is_empty()) =>
                    {
                        break;
                    }
                    Ok(_) | Err(_) => {
                        tokio::time::sleep(StdDuration::from_millis(25)).await;
                    }
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("backend restart round {round} must clear Redis leases"));
    }
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
        Some(redis_store),
    )
    .unwrap();
    let empty = HashSet::new();

    let mut first = manager_a
        .acquire_context_for_session(None, Some("shared-session"), &empty)
        .await
        .unwrap();
    let first_id = first.id;
    first.release_in_flight();

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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn redis_backed_temporary_sticky_capacity_fallback_rebounds_without_full_sync() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis sticky stale-state 测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };

    let mut config = Config::default();
    config.load_balancing_mode = "priority".to_string();
    config.credential_max_concurrent_requests = 1;
    let credentials = vec![
        api_key_credential("sticky-a"),
        api_key_credential("sticky-b"),
    ];
    let manager_a = MultiTokenManager::new_with_stores(
        config.clone(),
        credentials.clone(),
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();
    let manager_b = MultiTokenManager::new_with_stores(
        config,
        credentials,
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();
    let empty = HashSet::new();
    let session_id = "redis-sticky-stale-capacity";

    let mut initial = manager_a
        .acquire_context_for_session_with_mode(
            None,
            Some(session_id),
            &empty,
            AcquireMode::FailFastOnCapacity,
            1,
        )
        .await
        .unwrap();
    let bound_id = initial.id;
    initial.release_in_flight();
    tokio::time::timeout(StdDuration::from_secs(1), async {
        loop {
            let state = redis_store
                .scheduler_state_for_credentials(&[bound_id])
                .await
                .unwrap();
            if state
                .get(&bound_id)
                .is_none_or(|state| state.in_flight_leases.is_empty())
            {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("initial sticky lease should release before creating the holder");

    let holder = manager_a
        .acquire_context_for_session_with_mode(
            None,
            Some(session_id),
            &empty,
            AcquireMode::FailFastOnCapacity,
            1,
        )
        .await
        .unwrap();
    assert_eq!(holder.id, bound_id);
    manager_b
        .refresh_scheduler_state_from_redis_force()
        .unwrap();
    assert_eq!(
        manager_b
            .entries
            .lock()
            .iter()
            .find(|entry| entry.id == bound_id)
            .unwrap()
            .in_flight_requests,
        1,
        "secondary manager must start from a stale full snapshot for this regression"
    );

    let reads_before_fallback = manager_b
        .request_binding_snapshot_reads
        .load(Ordering::Acquire);
    let mut fallback = manager_b
        .acquire_context_for_session_with_mode(
            None,
            Some(session_id),
            &empty,
            AcquireMode::FailFastOnCapacity,
            1,
        )
        .await
        .unwrap();
    assert_ne!(fallback.id, bound_id);
    assert!(fallback.fallback_from_sticky);
    assert_eq!(
        manager_b
            .request_binding_snapshot_reads
            .load(Ordering::Acquire)
            .saturating_sub(reads_before_fallback),
        1,
        "Redis lease rejection and local reselect must not reread the binding"
    );
    assert_eq!(
        redis_store
            .get_session_binding(session_id)
            .await
            .unwrap()
            .unwrap()
            .credential_id,
        bound_id,
        "temporary capacity fallback must not migrate the authoritative binding"
    );
    fallback.release_in_flight();

    drop(holder);
    tokio::time::timeout(StdDuration::from_millis(700), async {
        loop {
            let state = redis_store
                .scheduler_state_for_credentials(&[bound_id])
                .await
                .unwrap();
            if state
                .get(&bound_id)
                .is_none_or(|state| state.in_flight_leases.is_empty())
            {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("holder Redis lease should be released before the 1s full-sync interval");
    assert_eq!(
        manager_b
            .entries
            .lock()
            .iter()
            .find(|entry| entry.id == bound_id)
            .unwrap()
            .in_flight_requests,
        1,
        "secondary local snapshot must still be stale when rebound is attempted"
    );

    let mut rebound = manager_b
        .acquire_context_for_session_with_mode(
            None,
            Some(session_id),
            &empty,
            AcquireMode::FailFastOnCapacity,
            1,
        )
        .await
        .unwrap();
    assert_eq!(
        rebound.id, bound_id,
        "Redis authority must permit immediate rebound despite stale local remote capacity"
    );
    rebound.release_in_flight();
    crate::kiro::token_manager::drain_best_effort_storage_tasks(StdDuration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn redis_backed_sticky_release_grace_keeps_binding_between_managers() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis sticky release grace 测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };

    let mut config = Config::default();
    config.load_balancing_mode = "priority".to_string();
    config.credential_max_concurrent_requests = 1;
    config.credential_dispatch_max_wait_secs = 1;
    let credentials = vec![
        api_key_credential("sticky-release-grace-a"),
        api_key_credential("sticky-release-grace-b"),
    ];
    let manager_a = MultiTokenManager::new_with_stores(
        config.clone(),
        credentials.clone(),
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();
    let manager_b = MultiTokenManager::new_with_stores(
        config,
        credentials,
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();
    let empty = HashSet::new();
    let session_id = format!(
        "redis-sticky-release-grace-{}",
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );

    let mut initial = manager_a
        .acquire_context_for_session_with_mode(
            None,
            Some(&session_id),
            &empty,
            AcquireMode::FailFastOnCapacityWaitForRedis(StdDuration::from_secs(1)),
            1,
        )
        .await
        .unwrap();
    let bound_id = initial.id;
    tokio::time::timeout(StdDuration::from_secs(1), async {
        loop {
            if redis_store
                .get_session_binding(&session_id)
                .await
                .unwrap()
                .is_some_and(|binding| binding.credential_id == bound_id)
            {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("initial Redis sticky binding must be visible before release propagation test");

    let release = tokio::spawn(async move {
        tokio::time::sleep(StdDuration::from_millis(10)).await;
        initial.release_in_flight();
        crate::kiro::token_manager::drain_best_effort_storage_tasks(StdDuration::from_secs(2))
            .await;
    });

    let mut rebound = manager_b
        .acquire_context_for_session_with_mode(
            None,
            Some(&session_id),
            &empty,
            AcquireMode::FailFastOnCapacityWaitForRedis(StdDuration::from_secs(1)),
            1,
        )
        .await
        .unwrap();
    assert_eq!(
        rebound.id, bound_id,
        "short cross-instance Redis release propagation lag must not migrate sticky sessions"
    );
    assert!(rebound.sticky_bound);
    assert!(!rebound.fallback_from_sticky);
    rebound.release_in_flight();
    release.await.unwrap();
    crate::kiro::token_manager::drain_best_effort_storage_tasks(StdDuration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn redis_backed_sticky_holder_still_falls_back_after_release_grace() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis sticky holder fallback grace 测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };

    let mut config = Config::default();
    config.load_balancing_mode = "priority".to_string();
    config.credential_max_concurrent_requests = 1;
    config.credential_dispatch_max_wait_secs = 1;
    let credentials = vec![
        api_key_credential("sticky-holder-grace-a"),
        api_key_credential("sticky-holder-grace-b"),
    ];
    let manager_a = MultiTokenManager::new_with_stores(
        config.clone(),
        credentials.clone(),
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();
    let manager_b = MultiTokenManager::new_with_stores(
        config,
        credentials,
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();
    let empty = HashSet::new();
    let session_id = format!(
        "redis-sticky-holder-grace-{}",
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );

    let holder = manager_a
        .acquire_context_for_session_with_mode(
            None,
            Some(&session_id),
            &empty,
            AcquireMode::FailFastOnCapacityWaitForRedis(StdDuration::from_secs(1)),
            1,
        )
        .await
        .unwrap();
    let bound_id = holder.id;

    let started = Instant::now();
    let mut fallback = manager_b
        .acquire_context_for_session_with_mode(
            None,
            Some(&session_id),
            &empty,
            AcquireMode::FailFastOnCapacityWaitForRedis(StdDuration::from_secs(1)),
            1,
        )
        .await
        .unwrap();
    assert!(
        started.elapsed() < StdDuration::from_millis(500),
        "real sticky holder fallback must remain bounded by the short grace, not the full dispatch wait"
    );
    assert_ne!(fallback.id, bound_id);
    assert!(fallback.fallback_from_sticky);
    assert!(!fallback.sticky_bound);
    assert_eq!(
        redis_store
            .get_session_binding(&session_id)
            .await
            .unwrap()
            .unwrap()
            .credential_id,
        bound_id,
        "temporary sticky holder fallback must not migrate the authoritative binding"
    );
    fallback.release_in_flight();
    drop(holder);
    crate::kiro::token_manager::drain_best_effort_storage_tasks(StdDuration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn redis_dispatch_waiters_share_one_scheduler_state_scan_for_five_rounds() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis waiter singleflight 测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };

    const ACCOUNT_COUNT: usize = 60;
    const WAITER_COUNT: usize = 64;
    let credentials = (0..ACCOUNT_COUNT)
        .map(|index| api_key_credential(&format!("waiter-singleflight-{index}")))
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

    for round in 0..5 {
        *manager.last_scheduler_redis_sync_at.lock() = None;
        manager
            .scheduler_redis_sync_in_flight
            .store(false, Ordering::Release);
        redis_store.reset_scheduler_state_round_trips();

        futures::future::join_all((0..WAITER_COUNT).map(|_| async {
            let mut waiter = manager.capacity_signal.register();
            manager
                .wait_for_dispatch_capacity(
                    Some(StdDuration::from_millis(1)),
                    Some(StdDuration::from_secs(1)),
                    &mut waiter,
                )
                .await;
        }))
        .await;
        tokio::time::timeout(StdDuration::from_secs(2), async {
            while manager
                .scheduler_redis_sync_in_flight
                .load(Ordering::Acquire)
            {
                tokio::time::sleep(StdDuration::from_millis(5)).await;
            }
        })
        .await
        .expect("coalesced scheduler state scan must complete");

        assert_eq!(
            redis_store.scheduler_state_round_trips(),
            ACCOUNT_COUNT.div_ceil(16) as u64,
            "round {round}: waiter count must not multiply Redis scheduler scans"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn provisional_local_reservation_spreads_concurrent_redis_acquires() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis provisional spread 测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };

    const ACCOUNT_COUNT: usize = 60;
    let mut config = Config::default();
    config.load_balancing_mode = "priority".to_string();
    config.credential_max_concurrent_requests = 1;
    let credentials = (0..ACCOUNT_COUNT)
        .map(|index| api_key_credential(&format!("provisional-{index}")))
        .collect();
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
    redis_store.reset_scheduler_state_round_trips();
    manager.refresh_scheduler_state_from_redis_force().unwrap();
    assert_eq!(
        redis_store.scheduler_state_round_trips(),
        4,
        "60-account scheduler sync must use bounded batches instead of 11 commands per account"
    );
    for contenders in [1usize, 8, 24, 48] {
        let barrier = Arc::new(tokio::sync::Barrier::new(contenders));
        let lease_id_before = manager.next_in_flight_lease_id.load(Ordering::Acquire);
        let mut tasks = Vec::with_capacity(contenders);
        for _ in 0..contenders {
            let manager = manager.clone();
            let barrier = barrier.clone();
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                manager
                    .acquire_context_for_session_with_mode(
                        None,
                        None,
                        &HashSet::new(),
                        AcquireMode::FailFastOnCapacity,
                        1,
                    )
                    .await
            }));
        }

        let mut contexts = Vec::with_capacity(contenders);
        for task in tasks {
            contexts.push(task.await.unwrap().unwrap());
        }
        let selected: HashSet<u64> = contexts.iter().map(|context| context.id).collect();
        assert_eq!(selected.len(), contenders);
        let attempts = manager
            .next_in_flight_lease_id
            .load(Ordering::Acquire)
            .saturating_sub(lease_id_before);
        let max_attempts = (contenders as u64 * 5).div_ceil(4);
        assert!(
            attempts <= max_attempts,
            "provisional acquire amplification exceeded 1.25x: concurrency={contenders}, attempts={attempts}, max={max_attempts}"
        );
        eprintln!(
            "provisional-spread concurrency={contenders} unique={} attempts={attempts} ratio={:.3}",
            selected.len(),
            attempts as f64 / contenders as f64
        );
        for context in &mut contexts {
            context.release_in_flight();
        }
        assert!(
            manager
                .drain_scheduler_redis_releases(StdDuration::from_secs(3))
                .await,
            "round with concurrency={contenders}: Redis release dispatcher must drain"
        );
        crate::kiro::token_manager::drain_best_effort_storage_tasks(StdDuration::from_secs(2))
            .await;
        assert!(
            redis_store
                .scheduler_state_for_credentials(&(1..=ACCOUNT_COUNT as u64).collect::<Vec<_>>())
                .await
                .unwrap()
                .values()
                .all(|state| state.in_flight_leases.is_empty())
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redis_rejected_provisional_acquire_rolls_back_without_remote_release() {
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis provisional rejection 测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let mut config = Config::default();
    config.credential_max_concurrent_requests = 1;
    let manager_a = MultiTokenManager::new_with_stores(
        config.clone(),
        vec![api_key_credential("reject-a")],
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();
    let manager_b = MultiTokenManager::new_with_stores(
        config,
        vec![api_key_credential("reject-b")],
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();

    let holder = manager_a
        .acquire_in_flight_slot(1, 1)
        .await
        .unwrap()
        .unwrap();
    assert!(
        manager_b
            .acquire_in_flight_slot(1, 1)
            .await
            .unwrap()
            .is_none()
    );
    let local = &manager_b.entries.lock()[0];
    assert_eq!(local.in_flight_requests, 0);
    assert!(local.in_flight_leases.is_empty());
    assert_eq!(
        redis_store
            .scheduler_state_for_credentials(&[1])
            .await
            .unwrap()
            .get(&1)
            .unwrap()
            .in_flight_leases
            .len(),
        1,
        "a definitive Redis rejection must not delete the holder lease"
    );
    drop(holder);
    crate::kiro::token_manager::drain_best_effort_storage_tasks(StdDuration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_provisional_redis_acquire_rolls_back_local_and_tombstones_remote() {
    if test_redis_toxiproxy().is_none() {
        eprintln!("跳过 Redis provisional cancel 测试：未设置 Toxiproxy 测试环境变量");
        return;
    }
    clear_test_redis_latency_toxic().await;
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis provisional cancel 测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let manager = Arc::new(
        MultiTokenManager::new_with_stores(
            Config::default(),
            vec![api_key_credential("cancel-provisional")],
            None,
            None,
            false,
            None,
            Some(redis_store.clone()),
        )
        .unwrap(),
    );
    set_test_redis_latency_toxic(150).await;

    let task_manager = manager.clone();
    let acquire = tokio::spawn(async move { task_manager.acquire_in_flight_slot(1, 1).await });
    tokio::time::timeout(StdDuration::from_millis(100), async {
        while manager.entries.lock()[0].in_flight_requests == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("provisional local reservation should be visible before cancellation");
    acquire.abort();
    assert!(acquire.await.unwrap_err().is_cancelled());
    assert_eq!(manager.entries.lock()[0].in_flight_requests, 0);
    assert!(manager.entries.lock()[0].in_flight_leases.is_empty());

    clear_test_redis_latency_toxic().await;
    let drained =
        crate::kiro::token_manager::drain_best_effort_storage_tasks(StdDuration::from_secs(5))
            .await;
    assert!(drained.drained, "cancel cleanup should drain: {drained:?}");
    tokio::time::timeout(StdDuration::from_secs(2), async {
        loop {
            let state = redis_store
                .scheduler_state_for_credentials(&[1])
                .await
                .unwrap();
            if state
                .get(&1)
                .is_none_or(|state| state.in_flight_leases.is_empty())
            {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("cancelled commit-unknown lease must be removed before TTL");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn redis_commit_unknown_provisional_acquire_leaves_no_lease() {
    if test_redis_toxiproxy().is_none() {
        eprintln!("跳过 Redis provisional timeout 测试：未设置 Toxiproxy 测试环境变量");
        return;
    }
    clear_test_redis_latency_toxic().await;
    let Some(redis_store) = test_redis_store().await else {
        eprintln!("跳过 Redis provisional timeout 测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let manager = MultiTokenManager::new_with_stores(
        Config::default(),
        vec![api_key_credential("timeout-provisional")],
        None,
        None,
        false,
        None,
        Some(redis_store.clone()),
    )
    .unwrap();
    set_test_redis_latency_toxic(500).await;

    let result = manager.acquire_in_flight_slot(1, 1).await;
    assert!(
        result.is_err(),
        "capacity Redis deadline must fail this request closed"
    );
    assert!(
        !manager.scheduler_redis_breaker.is_degraded(),
        "one slow capacity acquire must not immediately open the degraded breaker"
    );
    assert_eq!(manager.entries.lock()[0].in_flight_requests, 0);
    assert!(manager.entries.lock()[0].in_flight_leases.is_empty());

    clear_test_redis_latency_toxic().await;
    let drained =
        crate::kiro::token_manager::drain_best_effort_storage_tasks(StdDuration::from_secs(5))
            .await;
    assert!(drained.drained, "timeout cleanup should drain: {drained:?}");
    tokio::time::timeout(StdDuration::from_secs(2), async {
        loop {
            let state = redis_store
                .scheduler_state_for_credentials(&[1])
                .await
                .unwrap();
            if state
                .get(&1)
                .is_none_or(|state| state.in_flight_leases.is_empty())
            {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("timed-out commit-unknown lease must be removed before TTL");
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

#[tokio::test]
async fn test_deferred_session_soft_failure_and_unbind_use_local_state() {
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

    let mut bound = manager
        .acquire_context_for_session(None, Some("deferred-session"), &empty)
        .await
        .unwrap();
    assert!(!manager.record_session_soft_failure_deferred("deferred-session", bound.id));
    assert!(manager.record_session_soft_failure_deferred("deferred-session", bound.id));

    let mut excluded = HashSet::new();
    excluded.insert(bound.id);
    let mut fallback = manager
        .acquire_context_for_session(None, Some("deferred-session"), &excluded)
        .await
        .unwrap();
    assert_ne!(bound.id, fallback.id);

    manager.unbind_session_if_bound_to_deferred("deferred-session", fallback.id);
    fallback.release_in_flight();

    let mut rebound = manager
        .acquire_context_for_session(None, Some("deferred-session"), &empty)
        .await
        .unwrap();
    assert_eq!(
        bound.id, rebound.id,
        "deferred unbind must not remove a sticky binding owned by another credential"
    );

    rebound.release_in_flight();
    bound.release_in_flight();
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

    assert!(
        manager
            .report_risk_controlled_outcome(
                1,
                CredentialRiskControlReason::TemporarilySuspended,
                "TEMPORARILY_SUSPENDED"
            )
            .can_retry_local()
    );

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
async fn local_pool_risk_circuit_stops_burning_remaining_credentials() {
    let mut config = Config::default();
    config.external_pools.local_pool_circuit_enabled = true;
    config.external_pools.local_pool_circuit_open_after_failures = 2;
    config
        .external_pools
        .local_pool_circuit_require_distinct_credentials = 2;
    config.external_pools.local_pool_circuit_open_secs = 30;

    let manager = MultiTokenManager::new(
        config,
        vec![
            test_access_token_credential("risk-1", "Pro"),
            test_access_token_credential("risk-2", "Pro"),
            test_access_token_credential("risk-3", "Pro"),
        ],
        None,
        None,
        false,
    )
    .unwrap();

    let first = manager.report_risk_controlled_outcome(
        1,
        CredentialRiskControlReason::TemporarilySuspended,
        "TEMPORARILY_SUSPENDED first",
    );
    assert!(first.can_retry_local());
    assert!(!first.circuit_open);
    assert_eq!(manager.available_count(), 2);

    let second = manager.report_risk_controlled_outcome(
        2,
        CredentialRiskControlReason::TemporarilySuspended,
        "TEMPORARILY_SUSPENDED second",
    );
    assert!(
        second.has_available_credentials,
        "one untouched credential remains available"
    );
    assert!(second.circuit_open);
    assert!(!second.can_retry_local());
    assert!(second.retry_after_secs.is_some());
    assert_eq!(manager.available_count(), 1);

    let state = manager.local_pool_route_state(None);
    assert_eq!(state.kind, LocalPoolRouteStateKind::RiskCircuitOpen);
    assert_eq!(state.available, 1);
    assert_eq!(state.dispatchable, 0);
    assert!(state.retry_after_secs.is_some());

    let error = match manager.acquire_context(None).await {
        Ok(mut ctx) => {
            ctx.release_in_flight();
            panic!("risk circuit must stop local acquisition");
        }
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("本地账号池风险保护已打开"),
        "unexpected error: {error}"
    );
    let snapshot = manager.snapshot();
    let untouched = snapshot.entries.iter().find(|entry| entry.id == 3).unwrap();
    assert!(
        !untouched.disabled,
        "circuit must not disable untouched accounts"
    );
}

#[test]
fn runtime_capacity_updates_reset_active_credential_warmup() {
    let mut config = Config::default();
    config.credential_warmup_requests = 5;
    config.credential_rpm = Some(60);

    let manager = MultiTokenManager::new(
        config,
        vec![
            test_access_token_credential("warmup-global-1", "Pro"),
            test_access_token_credential("warmup-global-2", "Pro"),
        ],
        None,
        None,
        false,
    )
    .unwrap();
    {
        let mut entries = manager.entries.lock();
        entries[0].warmup_remaining = 0;
        entries[1].warmup_remaining = 0;
        entries[1].credentials.disabled = true;
        entries[1].disabled = true;
    }

    manager
        .update_runtime_config(|config| {
            config.credential_rpm = Some(90);
        })
        .unwrap();

    let entries = manager.entries.lock();
    assert_eq!(entries[0].warmup_remaining, 5);
    assert_eq!(
        entries[1].warmup_remaining, 0,
        "disabled credentials should not be reintroduced by global capacity warmup"
    );
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
