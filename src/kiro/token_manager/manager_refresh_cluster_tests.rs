use super::*;
use axum::response::IntoResponse;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

const CLUSTER_REFRESH_BUDGETS: TokenRefreshBudgets = TokenRefreshBudgets {
    workflow: StdDuration::from_secs(12),
    coordination: StdDuration::from_secs(6),
    reconciliation: StdDuration::from_secs(3),
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClusterEndpointMode {
    Success,
    ServerError,
    PendingFirstThenSuccess,
}

#[derive(Clone)]
struct ClusterRefreshEndpointState {
    hits: Arc<AtomicUsize>,
    mode: Arc<AtomicU64>,
    first_pending: Arc<AtomicBool>,
    request_received: Arc<tokio::sync::Notify>,
    release_pending: Arc<tokio::sync::Notify>,
    access_token: Arc<parking_lot::Mutex<String>>,
    refresh_token: Arc<parking_lot::Mutex<Option<String>>>,
}

impl ClusterRefreshEndpointState {
    fn new(mode: ClusterEndpointMode, access_token: String, refresh_token: Option<String>) -> Self {
        Self {
            hits: Arc::new(AtomicUsize::new(0)),
            mode: Arc::new(AtomicU64::new(mode as u64)),
            first_pending: Arc::new(AtomicBool::new(
                mode == ClusterEndpointMode::PendingFirstThenSuccess,
            )),
            request_received: Arc::new(tokio::sync::Notify::new()),
            release_pending: Arc::new(tokio::sync::Notify::new()),
            access_token: Arc::new(parking_lot::Mutex::new(access_token)),
            refresh_token: Arc::new(parking_lot::Mutex::new(refresh_token)),
        }
    }

    fn set_success(&self, access_token: String, refresh_token: Option<String>) {
        *self.access_token.lock() = access_token;
        *self.refresh_token.lock() = refresh_token;
        self.mode
            .store(ClusterEndpointMode::Success as u64, Ordering::Release);
        self.release_pending.notify_waiters();
    }

    fn mode(&self) -> ClusterEndpointMode {
        match self.mode.load(Ordering::Acquire) {
            value if value == ClusterEndpointMode::Success as u64 => ClusterEndpointMode::Success,
            value if value == ClusterEndpointMode::ServerError as u64 => {
                ClusterEndpointMode::ServerError
            }
            _ => ClusterEndpointMode::PendingFirstThenSuccess,
        }
    }
}

struct ClusterRefreshEndpoint {
    url: String,
    state: ClusterRefreshEndpointState,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for ClusterRefreshEndpoint {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn cluster_refresh_endpoint(
    axum::extract::State(state): axum::extract::State<ClusterRefreshEndpointState>,
) -> axum::response::Response {
    state.hits.fetch_add(1, Ordering::AcqRel);
    state.request_received.notify_waiters();
    if state.mode() == ClusterEndpointMode::PendingFirstThenSuccess
        && state.first_pending.swap(false, Ordering::AcqRel)
    {
        state.release_pending.notified().await;
    }
    if state.mode() == ClusterEndpointMode::ServerError {
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({ "error": "temporary_failure" })),
        )
            .into_response();
    }
    tokio::time::sleep(StdDuration::from_millis(40)).await;
    let access_token = state.access_token.lock().clone();
    let refresh_token = state.refresh_token.lock().clone();
    let mut body = serde_json::json!({
        "access_token": access_token,
        "expires_in": 3600,
        "scope": "offline_access codewhisperer:conversations",
    });
    if let Some(refresh_token) = refresh_token {
        body["refresh_token"] = serde_json::Value::String(refresh_token);
    }
    (axum::http::StatusCode::OK, axum::Json(body)).into_response()
}

async fn spawn_cluster_refresh_endpoint(
    mode: ClusterEndpointMode,
    access_token: String,
    refresh_token: Option<String>,
) -> ClusterRefreshEndpoint {
    let state = ClusterRefreshEndpointState::new(mode, access_token, refresh_token);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = axum::Router::new()
        .route("/token", axum::routing::post(cluster_refresh_endpoint))
        .with_state(state.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    ClusterRefreshEndpoint {
        url: format!("http://{address}/token"),
        state,
        task,
    }
}

async fn run_refresh_cluster_fixture<F, Fut>(body: F)
where
    F: FnOnce(Vec<Arc<PostgresStore>>, Vec<Arc<RedisStore>>) -> Fut,
    Fut: Future<Output = ()>,
{
    let Some(postgres_owner) = test_postgres_store().await else {
        eprintln!("跳过 refresh cluster 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };
    let postgres_url = crate::storage::integration_test_url("KIRO_RS_TEST_POSTGRES_URL")
        .expect("PostgreSQL URL was present for the owner store");
    let mut postgres_config = Config::default();
    postgres_config.postgres.url = Some(postgres_url);
    postgres_config.postgres.max_connections = 2;
    let postgres_peer = Arc::new(
        PostgresStore::connect_test_peer(&postgres_config, postgres_owner.as_ref())
            .await
            .unwrap(),
    );
    let Some(redis_stores) = test_redis_stores_with_shared_namespace(2).await else {
        postgres_owner.drop_test_schema().await.unwrap();
        eprintln!("跳过 refresh cluster 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let cleanup_redis = redis_stores[0].clone();
    let cleanup_postgres = postgres_owner.clone();
    let postgres_stores = vec![postgres_owner, postgres_peer];
    let outcome = AssertUnwindSafe(body(postgres_stores, redis_stores))
        .catch_unwind()
        .await;
    crate::kiro::token_manager::drain_best_effort_storage_tasks(StdDuration::from_secs(5)).await;
    let cleanup = cleanup_redis
        .delete_pattern_bounded("*", None)
        .await
        .unwrap();
    assert!(!cleanup.cancelled);
    assert!(!cleanup.pass_limit_reached);
    assert_eq!(
        cleanup_redis
            .delete_pattern_bounded("*", None)
            .await
            .unwrap()
            .deleted_keys,
        0
    );
    cleanup_postgres.drop_test_schema().await.unwrap();
    if let Err(payload) = outcome {
        std::panic::resume_unwind(payload);
    }
}

fn expired_cluster_credential(endpoint: String, marker: &str) -> KiroCredentials {
    KiroCredentials {
        auth_method: Some("external_idp".to_string()),
        access_token: Some(format!("old-access-{marker}")),
        refresh_token: Some(format!("refresh-{marker}-{}", "r".repeat(150))),
        client_id: Some(format!("cluster-client-{marker}")),
        machine_id: Some("0".repeat(64)),
        token_endpoint: Some(endpoint),
        scopes: Some("offline_access codewhisperer:conversations".to_string()),
        expires_at: Some((Utc::now() - Duration::hours(1)).to_rfc3339()),
        ..Default::default()
    }
}

fn cluster_managers(
    credential: &KiroCredentials,
    postgres: &[Arc<PostgresStore>],
    redis: &[Arc<RedisStore>],
) -> [Arc<MultiTokenManager>; 2] {
    [
        Arc::new(
            MultiTokenManager::new_with_stores(
                Config::default(),
                vec![credential.clone()],
                None,
                None,
                false,
                Some(postgres[0].clone()),
                Some(redis[0].clone()),
            )
            .unwrap(),
        ),
        Arc::new(
            MultiTokenManager::new_with_stores(
                Config::default(),
                vec![credential.clone()],
                None,
                None,
                false,
                Some(postgres[1].clone()),
                Some(redis[1].clone()),
            )
            .unwrap(),
        ),
    ]
}

fn refresh_identity_for_manager(manager: &MultiTokenManager, id: u64) -> RefreshAttemptIdentity {
    let credentials = {
        let entries = manager.entries.lock();
        entries
            .iter()
            .find(|entry| entry.id == id)
            .expect("cluster test credential must exist")
            .credentials
            .clone()
    };
    let credentials_for_proxy = manager
        .resolve_proxy_for_credential(credentials)
        .expect("cluster test credential proxy must resolve");
    let effective_proxy = credentials_for_proxy.effective_proxy(manager.proxy.as_ref());
    let config = manager.config.lock();
    RefreshAttemptIdentity::from_refresh_request(
        &credentials_for_proxy,
        &config,
        effective_proxy.as_ref(),
    )
}

fn assert_cluster_refresh_identity_stable(
    managers: &[Arc<MultiTokenManager>; 2],
    credential: &KiroCredentials,
    marker: &str,
) {
    let id = credential.id.unwrap();
    let first = refresh_identity_for_manager(managers[0].as_ref(), id);
    let second = refresh_identity_for_manager(managers[1].as_ref(), id);
    assert_eq!(
        first, second,
        "{marker}: manager refresh identity must match"
    );
}

async fn concurrent_cluster_refresh(
    managers: &[Arc<MultiTokenManager>; 2],
    credential: &KiroCredentials,
    update_health: bool,
) -> Vec<anyhow::Result<CallContext>> {
    let id = credential.id.unwrap();
    let start = Arc::new(tokio::sync::Barrier::new(3));
    let tasks = managers
        .iter()
        .map(|manager| {
            let manager = manager.clone();
            let credential = credential.clone();
            let start = start.clone();
            tokio::spawn(async move {
                start.wait().await;
                manager
                    .try_ensure_token_with_budgets(
                        id,
                        &credential,
                        update_health,
                        CLUSTER_REFRESH_BUDGETS,
                    )
                    .await
            })
        })
        .collect::<Vec<_>>();
    start.wait().await;
    futures::future::join_all(tasks)
        .await
        .into_iter()
        .map(|result| result.expect("cluster refresh task must not panic"))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn token_refresh_two_manager_rotating_and_non_rotating_share_one_send_and_pg_authority_for_five_rounds()
 {
    run_refresh_cluster_fixture(|postgres, redis| async move {
        for round in 1..=5 {
            for rotating in [false, true] {
                let mode = if rotating { "rotating" } else { "non-rotating" };
                let marker = format!("{mode}-{round}");
                let expected_access = format!("new-access-{marker}");
                let expected_refresh =
                    rotating.then(|| format!("new-refresh-{marker}-{}", "n".repeat(150)));
                let endpoint = spawn_cluster_refresh_endpoint(
                    ClusterEndpointMode::Success,
                    expected_access.clone(),
                    expected_refresh.clone(),
                )
                .await;
                let inserted = postgres[0]
                    .insert_credential(&expired_cluster_credential(endpoint.url.clone(), &marker))
                    .await
                    .unwrap();
                let old_refresh = inserted.refresh_token.clone();
                let managers = cluster_managers(&inserted, &postgres, &redis);
                assert_cluster_refresh_identity_stable(&managers, &inserted, &marker);
                let results = concurrent_cluster_refresh(&managers, &inserted, false).await;
                assert_eq!(endpoint.state.hits.load(Ordering::Acquire), 1, "{marker}");
                for (index, result) in results.into_iter().enumerate() {
                    let context = match result {
                        Ok(context) => context,
                        Err(error) => panic!("{marker} manager={index}: {error:#}"),
                    };
                    assert_eq!(context.token, expected_access, "{marker}");
                    assert_eq!(
                        context.credentials.access_token.as_deref(),
                        Some(expected_access.as_str())
                    );
                    assert_eq!(
                        context.credentials.refresh_token,
                        expected_refresh.clone().or_else(|| old_refresh.clone()),
                        "{marker}"
                    );
                }
                let stored = postgres[1]
                    .load_credentials()
                    .await
                    .unwrap()
                    .into_iter()
                    .find(|credential| credential.id == inserted.id)
                    .unwrap();
                assert_eq!(
                    stored.access_token.as_deref(),
                    Some(expected_access.as_str())
                );
                assert_eq!(
                    stored.refresh_token,
                    expected_refresh.or(old_refresh),
                    "{marker}"
                );
                assert!(
                    stored.storage_revision > inserted.storage_revision,
                    "{marker}"
                );
            }
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn token_refresh_two_manager_pg_cas_fences_stale_rotating_and_non_rotating_results_for_five_rounds()
 {
    run_refresh_cluster_fixture(|postgres, redis| async move {
        for round in 1..=5 {
            for rotating in [false, true] {
                let mode = if rotating { "rotating" } else { "non-rotating" };
                let marker = format!("cas-{mode}-{round}");
                let inserted = postgres[0]
                    .insert_credential(&expired_cluster_credential(
                        "http://127.0.0.1:1/not-called".to_string(),
                        &marker,
                    ))
                    .await
                    .unwrap();
                let managers = cluster_managers(&inserted, &postgres, &redis);
                assert_cluster_refresh_identity_stable(&managers, &inserted, &marker);
                let mut first = inserted.clone();
                first.access_token = Some(format!("first-access-{marker}"));
                first.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
                let mut second = inserted.clone();
                second.access_token = Some(format!("second-access-{marker}"));
                second.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
                if rotating {
                    first.refresh_token =
                        Some(format!("first-refresh-{marker}-{}", "a".repeat(150)));
                    second.refresh_token =
                        Some(format!("second-refresh-{marker}-{}", "b".repeat(150)));
                }
                let started = tokio::time::Instant::now();
                let deadline = started + StdDuration::from_secs(5);
                let (first_result, second_result) = tokio::join!(
                    managers[0].persist_refreshed_credential_fields(
                        inserted.id.unwrap(),
                        &inserted,
                        first,
                        true,
                        inserted.access_token.as_deref(),
                        deadline,
                        deadline,
                    ),
                    managers[1].persist_refreshed_credential_fields(
                        inserted.id.unwrap(),
                        &inserted,
                        second,
                        true,
                        inserted.access_token.as_deref(),
                        deadline,
                        deadline,
                    ),
                );
                let first_result =
                    first_result.unwrap_or_else(|error| panic!("{marker}: {error:#}"));
                let second_result =
                    second_result.unwrap_or_else(|error| panic!("{marker}: {error:#}"));
                let stored = postgres[0]
                    .load_credentials()
                    .await
                    .unwrap()
                    .into_iter()
                    .find(|credential| credential.id == inserted.id)
                    .unwrap();
                assert_eq!(
                    stored.storage_revision,
                    inserted.storage_revision + 1,
                    "{marker}"
                );
                assert_eq!(first_result.access_token, stored.access_token, "{marker}");
                assert_eq!(second_result.access_token, stored.access_token, "{marker}");
                assert_eq!(first_result.refresh_token, stored.refresh_token, "{marker}");
                assert_eq!(
                    second_result.refresh_token, stored.refresh_token,
                    "{marker}"
                );
            }
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn token_refresh_two_manager_failure_replay_and_cancelled_leader_recover_without_send_amplification_for_five_rounds()
 {
    run_refresh_cluster_fixture(|postgres, redis| async move {
        for round in 1..=5 {
            let marker = format!("failure-replay-{round}");
            let endpoint = spawn_cluster_refresh_endpoint(
                ClusterEndpointMode::ServerError,
                format!("recovered-access-{marker}"),
                None,
            )
            .await;
            let inserted = postgres[0]
                .insert_credential(&expired_cluster_credential(endpoint.url.clone(), &marker))
                .await
                .unwrap();
            let managers = cluster_managers(&inserted, &postgres, &redis);
            assert_cluster_refresh_identity_stable(&managers, &inserted, &marker);
            let results = concurrent_cluster_refresh(&managers, &inserted, false).await;
            assert_eq!(endpoint.state.hits.load(Ordering::Acquire), 1, "{marker}");
            for result in results {
                let error = match result {
                    Ok(_) => panic!("the shared 500 wave must fail"),
                    Err(error) => error,
                };
                let failure = error.downcast_ref::<RefreshFailure>().unwrap();
                assert_eq!(
                    failure.kind,
                    RefreshFailureKind::UpstreamUnavailable,
                    "{marker}"
                );
                assert!(failure.shared_failure_wave, "{marker}");
            }
            let immediate_result = managers[0]
                .try_ensure_token_with_budgets(
                    inserted.id.unwrap(),
                    &inserted,
                    false,
                    CLUSTER_REFRESH_BUDGETS,
                )
                .await;
            let immediate = match immediate_result {
                Ok(_) => panic!("the Redis failure result must replay immediately"),
                Err(error) => error,
            };
            assert!(
                immediate
                    .downcast_ref::<RefreshFailure>()
                    .unwrap()
                    .shared_failure_wave
            );
            assert_eq!(endpoint.state.hits.load(Ordering::Acquire), 1, "{marker}");
            endpoint
                .state
                .set_success(format!("recovered-access-{marker}"), None);
            tokio::time::sleep(StdDuration::from_secs(1)).await;
            let authoritative = postgres[0]
                .load_credentials()
                .await
                .unwrap()
                .into_iter()
                .find(|credential| credential.id == inserted.id)
                .unwrap();
            let recovered = concurrent_cluster_refresh(&managers, &authoritative, false).await;
            assert_eq!(endpoint.state.hits.load(Ordering::Acquire), 2, "{marker}");
            assert!(
                recovered.into_iter().all(|result| result.is_ok()),
                "{marker}"
            );

            let cancel_marker = format!("cancelled-leader-{round}");
            let pending = spawn_cluster_refresh_endpoint(
                ClusterEndpointMode::PendingFirstThenSuccess,
                format!("recovered-access-{cancel_marker}"),
                None,
            )
            .await;
            let pending_credential = postgres[0]
                .insert_credential(&expired_cluster_credential(
                    pending.url.clone(),
                    &cancel_marker,
                ))
                .await
                .unwrap();
            let pending_managers = cluster_managers(&pending_credential, &postgres, &redis);
            let leader = {
                let manager = pending_managers[0].clone();
                let credential = pending_credential.clone();
                tokio::spawn(async move {
                    manager
                        .try_ensure_token_with_budgets(
                            credential.id.unwrap(),
                            &credential,
                            false,
                            CLUSTER_REFRESH_BUDGETS,
                        )
                        .await
                })
            };
            tokio::time::timeout(
                StdDuration::from_secs(3),
                pending.state.request_received.notified(),
            )
            .await
            .expect("pending leader did not start its OAuth send");
            let follower = {
                let manager = pending_managers[1].clone();
                let credential = pending_credential.clone();
                tokio::spawn(async move {
                    manager
                        .try_ensure_token_with_budgets(
                            credential.id.unwrap(),
                            &credential,
                            false,
                            CLUSTER_REFRESH_BUDGETS,
                        )
                        .await
                })
            };
            tokio::time::sleep(StdDuration::from_millis(100)).await;
            leader.abort();
            let _ = leader.await;
            crate::kiro::token_manager::drain_best_effort_storage_tasks(StdDuration::from_secs(3))
                .await;
            let follower_result = tokio::time::timeout(StdDuration::from_secs(3), follower)
                .await
                .expect("follower did not observe cancelled leader outcome")
                .unwrap();
            let follower_error = match follower_result {
                Ok(_) => panic!("cancelled committed send must close the current wave"),
                Err(error) => error,
            };
            assert!(
                follower_error
                    .downcast_ref::<RefreshFailure>()
                    .unwrap()
                    .shared_failure_wave
            );
            assert_eq!(
                pending.state.hits.load(Ordering::Acquire),
                1,
                "{cancel_marker}"
            );
            pending
                .state
                .set_success(format!("recovered-access-{cancel_marker}"), None);
            tokio::time::sleep(StdDuration::from_secs(1)).await;
            let recovered =
                concurrent_cluster_refresh(&pending_managers, &pending_credential, false).await;
            assert_eq!(
                pending.state.hits.load(Ordering::Acquire),
                2,
                "{cancel_marker}"
            );
            assert!(
                recovered.into_iter().all(|result| result.is_ok()),
                "{cancel_marker}"
            );
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn token_refresh_two_manager_cancelled_health_claim_is_reclaimed_once_for_five_rounds() {
    run_refresh_cluster_fixture(|postgres, redis| async move {
        for round in 1_u64..=5 {
            let marker = format!("health-claim-{round}");
            let inserted = postgres[0]
                .insert_credential(&expired_cluster_credential(
                    "http://127.0.0.1:1/not-called".to_string(),
                    &marker,
                ))
                .await
                .unwrap();
            let id = inserted.id.unwrap();
            let managers = cluster_managers(&inserted, &postgres, &redis);
            let identity = RefreshAttemptIdentity::from_credentials(&inserted);
            let leader = match redis[0]
                .begin_token_refresh(id, identity.0, false)
                .await
                .unwrap()
            {
                RedisRefreshBegin::Leader(lease) => lease,
                other => panic!("{marker}: expected leader, got {other:?}"),
            };
            let failure = RedisRefreshFailure {
                stage: RedisRefreshFailureStage::ResponseStatus,
                kind: RedisRefreshFailureKind::RateLimited,
                status: Some(429),
                retry_after: Some(StdDuration::from_secs(10)),
                send_committed: true,
                health_action_required: true,
            };
            redis[0]
                .complete_token_refresh_failure(&leader, &failure, false)
                .await
                .unwrap()
                .expect("current leader must commit failure");
            let cancelled_claim = match redis[1]
                .begin_token_refresh(id, identity.0, true)
                .await
                .unwrap()
            {
                RedisRefreshBegin::Replay {
                    health_claim: Some(claim),
                    ..
                } => claim,
                other => panic!("{marker}: expected initial health claim, got {other:?}"),
            };
            let immediate = managers[1]
                .begin_distributed_refresh_until(
                    id,
                    identity,
                    true,
                    tokio::time::Instant::now() + StdDuration::from_secs(1),
                )
                .await
                .unwrap();
            assert!(
                matches!(immediate, DistributedRefreshDecision::Replay(_)),
                "{marker}"
            );
            assert!(
                managers[1].entries.lock()[0].cooldown_until.is_none(),
                "{marker}"
            );
            tokio::time::sleep(StdDuration::from_millis(5_200)).await;
            let reclaimed = managers[1]
                .begin_distributed_refresh_until(
                    id,
                    identity,
                    true,
                    tokio::time::Instant::now() + StdDuration::from_secs(2),
                )
                .await
                .unwrap();
            assert!(
                matches!(reclaimed, DistributedRefreshDecision::Replay(_)),
                "{marker}"
            );
            {
                let entries = managers[1].entries.lock();
                assert!(entries[0].cooldown_until.is_some(), "{marker}");
                assert!(!entries[0].disabled, "{marker}");
                assert_eq!(entries[0].failure_count, 0, "{marker}");
            }
            assert!(
                !redis[0]
                    .ack_token_refresh_health_claim(id, &cancelled_claim)
                    .await
                    .unwrap(),
                "{marker}: expired claim must not consume the reclaimed action"
            );
            let later = managers[0]
                .begin_distributed_refresh_until(
                    id,
                    identity,
                    true,
                    tokio::time::Instant::now() + StdDuration::from_secs(1),
                )
                .await
                .unwrap();
            assert!(
                matches!(later, DistributedRefreshDecision::Replay(_)),
                "{marker}"
            );
            assert!(
                managers[0].entries.lock()[0].cooldown_until.is_none(),
                "{marker}"
            );
        }
    })
    .await;
}
