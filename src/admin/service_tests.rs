fn normalize_admin_request_admission(
    config: crate::model::config::RequestAdmissionConfig,
) -> Result<crate::model::config::RequestAdmissionConfig, AdminServiceError> {
    config
        .validate()
        .map_err(AdminServiceError::InvalidCredential)?;
    Ok(config.normalized())
}
use super::*;
use crate::model::config::{AccountsConfig, RequestAdmissionConfig, RequestApiKeyPolicy};
use std::sync::Arc;

fn cleanup_request() -> UsageCleanupRequest {
    UsageCleanupRequest {
        mode: UsageCleanupMode::SoftDelete,
        include_summary: false,
        older_than_days: None,
        cutoff_before: None,
        batch_size: None,
        max_batches: None,
        max_rows: None,
        pause_ms_between_batches: None,
    }
}

#[test]
fn access_key_response_keeps_legacy_primary_field_and_lists_managed_policies() {
    let keys = vec![
        "sk-legacy-primary".to_string(),
        "sk-managed-client".to_string(),
    ];
    let policies = vec![RequestApiKeyPolicy {
        allowed_routes: Vec::new(),
        usage_limits: Vec::new(),
        api_key: "sk-managed-client".to_string(),
        name: "client-a".to_string(),
        enabled: true,
        request_admission: Some(RequestAdmissionConfig {
            max_concurrent_requests: 5,
            ..RequestAdmissionConfig::default()
        }),
    }];
    let response = access_keys_response(
        &keys,
        &policies,
        RequestAdmissionConfig::default(),
        "sk-admin",
    );

    assert_eq!(response.request_api_key, "sk-legacy-primary");
    assert_eq!(response.request_api_keys.len(), 2);
    assert!(response.request_api_keys[0].primary);
    assert_eq!(response.request_api_keys[0].api_key, "sk-legacy-primary");
    assert!(!response.request_api_keys[1].primary);
    assert_eq!(response.request_api_keys[1].name, "client-a");
    assert_eq!(
        response.request_api_keys[1]
            .request_admission
            .unwrap()
            .max_concurrent_requests,
        5
    );

    let wire = serde_json::to_value(response).expect("access-key response json");
    assert_eq!(wire["requestApiKey"], "sk-legacy-primary");
    assert_eq!(wire["requestApiKeys"][1]["apiKey"], "sk-managed-client");
}

#[test]
fn admin_request_key_id_matches_runtime_authenticated_identity() {
    let items = request_api_key_items(&[" sk-one ".to_string()], &[]);
    let store = RequestApiKeyStore::new(["sk-one"]);
    let runtime_id = store.authenticate("sk-one").unwrap().stable_id();

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, runtime_id);
    assert_eq!(items[0].id.len(), 64);
    assert_eq!(
        items[0].id,
        crate::common::auth::request_api_key_id("sk-one")
    );
}

#[test]
fn audit_detail_normalization_is_bounded_without_returning_an_error() {
    let detail = AdminService::normalize_audit_detail(serde_json::json!({
        "apiKey": "secret-value",
        "supportedModels": (0..50).map(|index| format!("model-{index}")).collect::<Vec<_>>(),
    }));
    assert!(!detail.to_string().contains("secret-value"));
    assert_eq!(detail["supportedModels"]["count"], 50);
    assert_eq!(detail["supportedModels"]["truncated"], true);
}

#[test]
fn account_cost_floor_margin_validation_is_bounded() {
    let mut config = AccountsConfig {
        usage_projection_cost_floor_margin_percent: 200,
        ..AccountsConfig::default()
    };
    validate_accounts_config(&config).expect("upper bound should be accepted");

    config.usage_projection_cost_floor_margin_percent = 201;
    let err = validate_accounts_config(&config).unwrap_err();
    assert!(
        err.contains("usageProjectionCostFloorMarginPercent"),
        "unexpected validation error: {err}"
    );
}

#[test]
fn account_degrade_error_rate_threshold_is_a_ratio() {
    let mut config = AccountsConfig::default();
    for valid in [0.0, 0.5, 1.0] {
        config.degrade_error_rate_threshold = valid;
        validate_accounts_config(&config).expect("合法比率必须被接受");
    }
    for invalid in [-0.1, 1.1] {
        config.degrade_error_rate_threshold = invalid;
        let err = validate_accounts_config(&config).unwrap_err();
        assert!(
            err.contains("degradeErrorRateThreshold"),
            "unexpected validation error: {err}"
        );
    }
}

#[test]
fn account_probation_ceiling_cannot_be_below_single_probation() {
    // 避让上限小于单次避让时长会让指数退避的上限语义自相矛盾。
    let mut config = AccountsConfig {
        degrade_probation_secs: 600,
        max_probation_secs: 300,
        ..Default::default()
    };
    let err = validate_accounts_config(&config).unwrap_err();
    assert!(
        err.contains("maxProbationSecs"),
        "unexpected validation error: {err}"
    );

    config.max_probation_secs = 600;
    validate_accounts_config(&config).expect("相等必须被接受");
}

#[test]
fn account_quality_defaults_pass_validation() {
    // Quality-aware scheduling changes runtime decisions and must be opt-in on
    // upgrades. The remaining defaults still need to validate as a whole.
    let config = AccountsConfig::default();
    assert!(
        !config.quality_aware_scheduling_enabled,
        "质量感知调度主开关必须默认关闭"
    );
    validate_accounts_config(&config).expect("默认配置必须通过校验");
}

#[test]
fn account_quality_ewma_alpha_validation_is_bounded() {
    let mut config = AccountsConfig {
        quality_ewma_alpha: 0.01,
        ..Default::default()
    };
    validate_accounts_config(&config).expect("下界必须被接受");
    config.quality_ewma_alpha = 1.0;
    validate_accounts_config(&config).expect("上界必须被接受");

    for invalid in [0.0, 1.0001, -0.5, f64::NAN] {
        config.quality_ewma_alpha = invalid;
        let err = validate_accounts_config(&config).unwrap_err();
        assert!(
            err.contains("qualityEwmaAlpha"),
            "alpha={invalid} 应被拒绝，实际错误: {err}"
        );
    }
}

#[test]
fn account_quality_probe_share_and_top_k_are_bounded() {
    let config = AccountsConfig {
        probe_share_percent: 101,
        ..Default::default()
    };
    let err = validate_accounts_config(&config).unwrap_err();
    assert!(err.contains("probeSharePercent"), "{err}");

    let config = AccountsConfig {
        quality_top_k: 101,
        ..Default::default()
    };
    let err = validate_accounts_config(&config).unwrap_err();
    assert!(err.contains("qualityTopK"), "{err}");
}

#[test]
fn account_quality_weights_reject_negative_and_non_finite() {
    // 负权重会把"差"变成"好"，是最危险的误配置方向。
    for mutate in [
        |config: &mut AccountsConfig, value: f64| config.quality_error_weight = value,
        |config: &mut AccountsConfig, value: f64| config.quality_latency_weight = value,
        |config: &mut AccountsConfig, value: f64| config.quality_probation_weight = value,
        |config: &mut AccountsConfig, value: f64| config.quality_priority_weight = value,
        |config: &mut AccountsConfig, value: f64| config.quality_load_weight = value,
    ] {
        for invalid in [-1.0, f64::NAN, f64::INFINITY, 1_000_001.0] {
            let mut config = AccountsConfig::default();
            mutate(&mut config, invalid);
            let err =
                validate_accounts_config(&config).expect_err(&format!("权重 {invalid} 必须被拒绝"));
            assert!(
                err.contains("quality"),
                "unexpected validation error: {err}"
            );
        }
    }
}

#[test]
fn account_supported_models_canonicalize_legacy_claude_35_commands() {
    assert_eq!(
        AdminService::normalize_account_supported_models(vec![
            "claude-3-5-sonnet-20241022".to_string(),
            "claude-3.5-sonnet".to_string(),
            "claude-3-5-haiku-20241022-thinking[1m]".to_string(),
            "claude-3.5-haiku".to_string(),
            "claude-3-5-opus-20241022".to_string(),
            "tenant-claude-3-5-sonnet".to_string(),
        ]),
        vec![
            "sonnet-3.5".to_string(),
            "haiku-3.5-thinking[1m]".to_string(),
            "haiku-3.5".to_string(),
            "claude-3-5-opus-20241022".to_string(),
            "tenant-claude-3-5-sonnet".to_string(),
        ]
    );
}

#[test]
fn account_supported_models_use_claude_code_family_commands() {
    assert_eq!(
        AdminService::normalize_account_supported_models(vec![
            "claude-sonnet-4-5-20250929".to_string(),
            "claude-haiku-4.5".to_string(),
            "claude-opus-4-6-thinking".to_string(),
            "tenant-haiku-model".to_string(),
            "QWEN3-CODER-NEXT".to_string(),
        ]),
        vec![
            "sonnet-4.5".to_string(),
            "haiku-4.5".to_string(),
            "opus-4.6-thinking".to_string(),
            "tenant-haiku-model".to_string(),
            "qwen3-coder-next".to_string(),
        ]
    );
}

#[test]
fn account_transient_failure_priority_penalty_validation_is_bounded() {
    let mut config = AccountsConfig {
        transient_failure_priority_penalty: 10_000,
        ..AccountsConfig::default()
    };
    validate_accounts_config(&config).expect("upper bound should be accepted");

    config.transient_failure_priority_penalty = 10_001;
    let err = validate_accounts_config(&config).unwrap_err();
    assert!(
        err.contains("transientFailurePriorityPenalty"),
        "unexpected validation error: {err}"
    );
}

#[test]
fn account_wire_debug_validation_is_bounded() {
    let mut config = AccountsConfig {
        wire_debug_max_body_bytes: 1024 * 1024,
        wire_debug_max_files: 100_000,
        ..AccountsConfig::default()
    };
    validate_accounts_config(&config).expect("upper bounds should be accepted");

    config.wire_debug_max_body_bytes = 1024 * 1024 + 1;
    let err = validate_accounts_config(&config).unwrap_err();
    assert!(
        err.contains("wireDebugMaxBodyBytes"),
        "unexpected validation error: {err}"
    );

    config.wire_debug_max_body_bytes = 1024 * 1024;
    config.wire_debug_max_files = 100_001;
    let err = validate_accounts_config(&config).unwrap_err();
    assert!(
        err.contains("wireDebugMaxFiles"),
        "unexpected validation error: {err}"
    );
}

#[test]
fn extracts_model_ids_from_anthropic_and_openai_models_response() {
    let body = r#"{
            "data": [
                {"id": "claude-sonnet-5"},
                {"id": "claude-opus-4-8"}
            ]
        }"#;

    assert_eq!(
        extract_model_ids_from_models_response(body),
        vec!["claude-sonnet-5", "claude-opus-4-8"]
    );
}

#[test]
fn extracts_model_ids_from_pool_compatible_models_response() {
    let body = r#"{
            "defaultModel": {"modelId": "auto"},
            "models": [
                {"modelId": "claude-sonnet-4.5"},
                {"model": "claude-opus-4.8"},
                "claude-haiku-4.5"
            ]
        }"#;

    assert_eq!(
        extract_model_ids_from_models_response(body),
        vec![
            "claude-sonnet-4.5",
            "claude-opus-4.8",
            "claude-haiku-4.5",
            "auto",
        ]
    );
}

#[test]
fn remove_request_api_key_by_id_rejects_missing_key() {
    let mut keys = vec!["sk-one".to_string(), "sk-two".to_string()];

    let err = remove_request_api_key_by_id(
        &mut keys,
        &crate::common::auth::request_api_key_id("sk-missing"),
    )
    .expect_err("missing key should fail");

    assert!(matches!(err, AdminServiceError::InvalidCredential(_)));
    assert_eq!(keys, vec!["sk-one".to_string(), "sk-two".to_string()]);
}

#[test]
fn remove_request_api_key_by_id_rejects_removing_last_key() {
    let mut keys = vec!["sk-one".to_string()];

    let err = remove_request_api_key_by_id(
        &mut keys,
        &crate::common::auth::request_api_key_id("sk-one"),
    )
    .expect_err("last key should not be removable");

    assert!(matches!(err, AdminServiceError::Conflict(_)));
    assert_eq!(keys, vec!["sk-one".to_string()]);
}

#[test]
fn remove_request_api_key_by_id_removes_requested_key() {
    let mut keys = vec!["sk-one".to_string(), "sk-two".to_string()];

    let removed = remove_request_api_key_by_id(
        &mut keys,
        &crate::common::auth::request_api_key_id("sk-one"),
    )
    .expect("key should be removed");

    assert_eq!(removed, "sk-one");
    assert_eq!(keys, vec!["sk-two".to_string()]);
}

#[test]
fn request_admission_admin_save_refresh_returns_canonical_queue_fields() {
    let queue_only = RequestAdmissionConfig {
        rpm: 0,
        max_concurrent_requests: 0,
        max_queued_requests: 64,
        queue_timeout_ms: 1_000,
    };
    let saved = normalize_admin_request_admission(queue_only).expect("Admin save");
    assert_eq!(saved, RequestAdmissionConfig::disabled());
    assert!(!saved.enabled());

    let half_disabled_queue = RequestAdmissionConfig {
        rpm: 300,
        max_concurrent_requests: 32,
        max_queued_requests: 0,
        queue_timeout_ms: 1_000,
    };
    let saved = normalize_admin_request_admission(half_disabled_queue).expect("Admin save");
    assert_eq!(saved.max_queued_requests, 0);
    assert_eq!(saved.queue_timeout_ms, 0);
    assert!(saved.enabled());

    // Runtime GET applies the same idempotent normalization before returning the value.
    assert_eq!(saved.normalized(), saved);
}

#[test]
fn request_key_policy_validation_preserves_legacy_keys_and_fails_closed() {
    let keys = vec!["sk-legacy".to_string(), "sk-managed".to_string()];
    assert!(validate_request_api_key_policies(&keys, &[]).is_ok());

    let policy = RequestApiKeyPolicy {
        allowed_routes: Vec::new(),
        usage_limits: Vec::new(),
        api_key: "sk-managed".to_string(),
        name: "client-a".to_string(),
        enabled: true,
        request_admission: Some(RequestAdmissionConfig {
            max_concurrent_requests: 5,
            ..RequestAdmissionConfig::default()
        }),
    };
    assert!(validate_request_api_key_policies(&keys, std::slice::from_ref(&policy)).is_ok());

    let mut unknown = policy.clone();
    unknown.api_key = "sk-not-configured".to_string();
    assert!(validate_request_api_key_policies(&keys, &[unknown]).is_err());

    let mut disabled = policy.clone();
    disabled.enabled = false;
    assert!(validate_request_api_key_policies(&["sk-managed".to_string()], &[disabled]).is_err());

    assert!(validate_request_api_key_policies(&keys, &[policy.clone(), policy]).is_err());
}

#[test]
fn runtime_config_audit_array_summary_survives_normalization() {
    let detail = AdminService::normalize_audit_detail(serde_json::json!({
        "kind": "runtime_config_change",
        "changes": [{
            "path": "endpointFallbackOrder",
            "before": {"count": 0, "items": []},
            "after": {"count": 2, "items": ["first", "second"]},
        }],
    }));
    assert_eq!(
        detail["changes"][0]["before"]["items"],
        serde_json::json!([])
    );
    assert_eq!(
        detail["changes"][0]["after"]["items"],
        serde_json::json!(["first", "second"])
    );
}

#[test]
fn runtime_config_audit_detail_redacts_secret_values_and_bounds_arrays() {
    let mut before = crate::model::config::Config::default();
    before.api_keys = vec!["old-secret".to_string()];
    let mut after = before.clone();
    after.api_keys = (0..25).map(|index| format!("key-{index}")).collect();

    let detail = AdminService::runtime_config_audit_detail(&before, &after);
    let changes = detail["changes"].as_array().unwrap();
    let keys = changes
        .iter()
        .find(|change| change["path"] == "apiKeys")
        .unwrap();
    assert_eq!(keys["before"], "[已隐藏]");
    assert_eq!(keys["after"], "[已隐藏]");
    assert_eq!(keys["redacted"], true);
    assert!(!detail.to_string().contains("old-secret"));
    assert!(!detail.to_string().contains("key-24"));
}

#[test]
fn usage_cleanup_include_summary_is_simple_full_history_mode() {
    let before = Utc::now();
    let mut request = cleanup_request();
    request.mode = UsageCleanupMode::HardDelete;
    request.include_summary = true;
    request.older_than_days = Some(30);
    request.max_rows = Some(25);

    let plan = normalize_usage_cleanup_request(request).expect("summary cleanup is valid");
    let after = Utc::now();

    assert!(plan.include_summary);
    assert_eq!(plan.mode, UsageCleanupMode::SoftDelete);
    assert_eq!(plan.max_rows, None);
    assert!(plan.cutoff >= before - ChronoDuration::seconds(1));
    assert!(plan.cutoff <= after + ChronoDuration::seconds(1));
}

#[tokio::test]
async fn usage_cleanup_lease_renewal_recovers_from_transient_row_lock_for_three_rounds() {
    let Some(url) = crate::storage::integration_test_url("CLAUDE_POOL_TEST_POSTGRES_URL") else {
        eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
        return;
    };
    let mut config = crate::model::config::Config::default();
    config.postgres.url = Some(url);
    // Three connections, not two: one holds the injected row lock, one is left mid-UPDATE when
    // a timed-out attempt drops its future (sqlx must then drain that statement, which itself
    // waits for the lock, plus the after_release probe and a ping before the connection is
    // usable again), and the third lets the next attempt run as soon as the lock is released
    // instead of queueing behind that drain.
    config.postgres.max_connections = 3;
    let postgres = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
    let usage_store = PostgresUsageStore::new(postgres.clone());

    for round in 0..3 {
        sqlx::query("TRUNCATE TABLE usage_cleanup_jobs")
            .execute(postgres.pool())
            .await
            .unwrap();
        let job_id = format!("cleanup-heartbeat-round-{round}");
        let worker_id = format!("heartbeat-worker-{round}");
        usage_store
            .create_cleanup_job(NewUsageCleanupJob {
                job_id: &job_id,
                mode: "soft_delete",
                include_summary: false,
                cutoff_at: Utc::now() - ChronoDuration::days(7),
                batch_size: USAGE_CLEANUP_DEFAULT_BATCH_SIZE,
                max_batches: 100,
                max_rows: None,
                pause_ms_between_batches: 10,
            })
            .await
            .unwrap();
        usage_store
            .claim_cleanup_job(&job_id, &worker_id, 30)
            .await
            .unwrap()
            .expect("queued cleanup job must be claimable");

        let mut blocker = postgres.pool().begin().await.unwrap();
        sqlx::query("SELECT job_id FROM usage_cleanup_jobs WHERE job_id = $1 FOR UPDATE")
            .bind(&job_id)
            .fetch_one(&mut *blocker)
            .await
            .unwrap();

        // Timeline: attempt 1 holds for the whole attempt timeout (the row is locked), the
        // retry delay passes, attempt 2 starts, and only then is the lock released. Attempt 2
        // must complete its UPDATE inside its own timeout, so the attempt timeout has to cover
        // a Postgres round trip on a starved host, not just scheduler jitter: with 40 ms the
        // test failed 4 of 6 runs next to 24 busy-loop processes, and even 200 ms failed 5 of 8
        // because a lock-free UPDATE took up to 450 ms there. Size it like production (2 s
        // attempt timeout) rather than like a unit test; the retry logic under test is the
        // same either way.
        let attempt_timeout = StdDuration::from_millis(1_000);
        let retry_delay = StdDuration::from_millis(50);
        let release_after = StdDuration::from_millis(1_100);
        let renewal_store = usage_store.clone();
        let renewal_job_id = job_id.clone();
        let renewal_worker_id = worker_id.clone();
        let started = Instant::now();
        let renewal = tokio::spawn(async move {
            renew_usage_cleanup_lease_with_retry(
                &renewal_store,
                &renewal_job_id,
                &renewal_worker_id,
                UsageCleanupHeartbeatPolicy {
                    lease_secs: 30,
                    attempt_timeout,
                    max_attempts: 3,
                    retry_delay,
                },
            )
            .await
        });

        tokio::time::sleep(release_after).await;
        blocker.rollback().await.unwrap();
        let renewed = renewal.await.unwrap().unwrap();
        let elapsed = started.elapsed();
        assert_eq!(renewed, Some(false), "round {round}");
        assert!(
            elapsed >= attempt_timeout,
            "round {round}: the injected row lock must affect at least one attempt: {elapsed:?}"
        );
        // Three attempts plus both back-off delays (50 ms, 100 ms) is the whole retry budget;
        // anything beyond it would mean the heartbeat hung instead of giving up.
        assert!(
            elapsed < attempt_timeout * 3 + retry_delay * 3 + StdDuration::from_millis(500),
            "round {round}: heartbeat retry must stay bounded: {elapsed:?}"
        );

        let lease_owner: Option<String> = sqlx::query_scalar(
            "SELECT lease_owner FROM usage_cleanup_jobs WHERE job_id = $1 AND lease_until > now()",
        )
        .bind(&job_id)
        .fetch_one(postgres.pool())
        .await
        .unwrap();
        assert_eq!(
            lease_owner.as_deref(),
            Some(worker_id.as_str()),
            "round {round}"
        );
    }

    postgres.drop_test_schema().await.unwrap();
}

#[test]
fn usage_cleanup_lock_contention_backoff_is_bounded() {
    assert_eq!(
        usage_cleanup_lock_contention_backoff(1),
        StdDuration::from_millis(25)
    );
    assert_eq!(
        usage_cleanup_lock_contention_backoff(2),
        StdDuration::from_millis(50)
    );
    assert_eq!(
        usage_cleanup_lock_contention_backoff(5),
        StdDuration::from_millis(400)
    );
    assert_eq!(
        usage_cleanup_lock_contention_backoff(100),
        StdDuration::from_millis(400)
    );
}

#[test]
fn usage_cleanup_request_cutoff_before_overrides_days() {
    let cutoff = Utc::now() - ChronoDuration::days(30);
    let mut request = cleanup_request();
    request.mode = UsageCleanupMode::HardDelete;
    request.older_than_days = Some(1);
    request.cutoff_before = Some(cutoff.to_rfc3339());
    request.batch_size = Some(USAGE_CLEANUP_MAX_BATCH_SIZE);
    request.max_batches = Some(10_000);
    request.max_rows = Some(123);
    request.pause_ms_between_batches = Some(0);

    let plan = normalize_usage_cleanup_request(request).expect("valid request");

    assert_eq!(plan.mode, UsageCleanupMode::HardDelete);
    assert_eq!(plan.cutoff, cutoff);
    assert_eq!(plan.batch_size, USAGE_CLEANUP_MAX_BATCH_SIZE);
    assert_eq!(plan.max_batches, 10_000);
    assert_eq!(plan.max_rows, Some(123));
    assert_eq!(plan.pause_ms_between_batches, 0);

    let mut above_legacy_limit = cleanup_request();
    above_legacy_limit.batch_size = Some(501);
    let plan =
        normalize_usage_cleanup_request(above_legacy_limit).expect("501 is within current limit");
    assert_eq!(plan.batch_size, 501);
}

#[test]
fn usage_cleanup_request_rejects_unsafe_bounds() {
    let mut large_batch = cleanup_request();
    large_batch.batch_size = Some(USAGE_CLEANUP_MAX_BATCH_SIZE + 1);
    assert!(matches!(
        normalize_usage_cleanup_request(large_batch),
        Err(AdminServiceError::InvalidCredential(_))
    ));

    let mut too_many_batches = cleanup_request();
    too_many_batches.max_batches = Some(USAGE_CLEANUP_MAX_BATCHES + 1);
    assert!(matches!(
        normalize_usage_cleanup_request(too_many_batches),
        Err(AdminServiceError::InvalidCredential(_))
    ));

    let mut future_cutoff = cleanup_request();
    future_cutoff.cutoff_before = Some((Utc::now() + ChronoDuration::minutes(1)).to_rfc3339());
    assert!(matches!(
        normalize_usage_cleanup_request(future_cutoff),
        Err(AdminServiceError::InvalidCredential(_))
    ));

    let mut too_many_rows = cleanup_request();
    too_many_rows.max_rows = Some(USAGE_CLEANUP_MAX_ROWS + 1);
    assert!(matches!(
        normalize_usage_cleanup_request(too_many_rows),
        Err(AdminServiceError::InvalidCredential(_))
    ));
}

#[test]
fn usage_cleanup_request_uses_safe_manual_defaults() {
    let before = Utc::now();
    let plan = normalize_usage_cleanup_request(cleanup_request()).expect("valid request");
    let after = Utc::now();

    assert_eq!(plan.mode, UsageCleanupMode::SoftDelete);
    assert!(!plan.include_summary);
    assert_eq!(plan.batch_size, USAGE_CLEANUP_DEFAULT_BATCH_SIZE);
    assert_eq!(plan.max_batches, USAGE_CLEANUP_DEFAULT_MAX_BATCHES);
    assert_eq!(plan.max_rows, None);
    assert_eq!(plan.pause_ms_between_batches, 10);
    assert!(plan.cutoff >= before - ChronoDuration::seconds(1));
    assert!(plan.cutoff <= after + ChronoDuration::seconds(1));
}

#[test]
fn usage_cleanup_request_zero_days_uses_execution_cutoff() {
    let before = Utc::now();
    let mut zero_days = cleanup_request();
    zero_days.older_than_days = Some(0);

    let plan = normalize_usage_cleanup_request(zero_days).expect("zero days is valid");
    let after = Utc::now();

    assert!(plan.cutoff >= before - ChronoDuration::seconds(1));
    assert!(plan.cutoff <= after + ChronoDuration::seconds(1));
}

#[test]
fn usage_cleanup_resume_gets_a_fresh_per_run_batch_budget() {
    assert!(usage_cleanup_run_batch_limit_reached(10, 0, 10));
    assert!(!usage_cleanup_run_batch_limit_reached(10, 10, 10));
    assert!(!usage_cleanup_run_batch_limit_reached(19, 10, 10));
    assert!(usage_cleanup_run_batch_limit_reached(20, 10, 10));
}

#[test]
fn usage_cleanup_row_limit_is_a_per_run_budget() {
    // A paused job that already used its row budget gets a fresh one when resumed.
    assert_eq!(usage_cleanup_run_rows_remaining(10_000, 0, 10_000), 0);
    assert_eq!(
        usage_cleanup_run_rows_remaining(10_000, 10_000, 10_000),
        10_000
    );
    assert_eq!(
        usage_cleanup_run_rows_remaining(14_000, 10_000, 10_000),
        6_000
    );
    assert_eq!(usage_cleanup_run_rows_remaining(25_000, 10_000, 10_000), 0);
}

#[test]
fn runtime_config_rejects_invalid_common_settings_before_install() {
    let invalid_cases: Vec<Box<dyn Fn(&mut Config)>> = vec![
        Box::new(|config| config.prompt_cache_target_read_ratio = 1.0),
        Box::new(|config| config.prompt_cache_token_scale = 0.5),
        Box::new(|config| {
            config.prompt_cache_cap_jitter_min_tokens =
                config.prompt_cache_cap_jitter_max_tokens + 1
        }),
        Box::new(|config| config.prompt_cache_entry_ttl_secs = 0),
        Box::new(|config| config.inference_upstream_max_attempts = 0),
        Box::new(|config| config.audit_log_retention_days = 0),
        Box::new(|config| config.usage_record_retention_days = 0),
        Box::new(|config| config.high_cache_threshold = -1),
        Box::new(|config| config.routes.routes.clear()),
        Box::new(|config| config.request_admission.rpm = u32::MAX),
    ];
    for invalidate in invalid_cases {
        let mut config = Config::default();
        invalidate(&mut config);
        assert!(normalize_and_validate_runtime_config(&mut config).is_err());
    }
}

#[test]
fn runtime_config_normalizes_custom_routes_and_installs_defaults() {
    let mut config = Config::default();
    config
        .routes
        .routes
        .push(crate::model::routes::RouteConfig::new(" dfcache/example/ "));
    normalize_and_validate_runtime_config(&mut config).unwrap();
    assert!(
        config
            .routes
            .routes
            .iter()
            .any(|route| route.prefix == "/dfcache/example")
    );
    config
        .routes
        .routes
        .push(crate::model::routes::RouteConfig::new("/dfcache/example"));
    assert!(normalize_and_validate_runtime_config(&mut config).is_err());
}

// ============ 批次 E：校验、脱敏与审计 ============

#[test]
fn quality_history_window_follows_the_query_window() {
    let now = DateTime::parse_from_rfc3339("2026-11-19T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    // 没给 from：最近 hours 小时，最多 7 天。
    let (since, until) = resolve_quality_history_window(now, 24, None, None).unwrap();
    assert_eq!(until, now);
    assert_eq!(now - since, ChronoDuration::hours(24));
    let (since, _) = resolve_quality_history_window(now, 10_000, None, None).unwrap();
    assert_eq!(now - since, ChronoDuration::days(7));
    // 给了窗口：与查询窗口一致，终点不晚于现在，最长 7 天。
    let (since, until) = resolve_quality_history_window(
        now,
        24,
        Some("2026-11-18T00:00:00Z".into()),
        Some("2026-11-18T06:00:00Z".into()),
    )
    .unwrap();
    assert_eq!(since.to_rfc3339(), "2026-11-18T00:00:00+00:00");
    assert_eq!(until.to_rfc3339(), "2026-11-18T06:00:00+00:00");
    let (since, until) = resolve_quality_history_window(
        now,
        24,
        Some("2026-10-01T00:00:00Z".into()),
        Some("2026-12-01T00:00:00Z".into()),
    )
    .unwrap();
    assert_eq!(until, now);
    assert_eq!(until - since, ChronoDuration::days(7));
    assert!(resolve_quality_history_window(now, 24, Some("bad".into()), None).is_err());
    assert!(
        resolve_quality_history_window(
            now,
            24,
            Some("2026-11-18T06:00:00Z".into()),
            Some("2026-11-18T00:00:00Z".into())
        )
        .is_err()
    );
}

#[test]
fn proxy_password_update_keeps_secret_when_client_echoes_the_mask() {
    let secret = "super-secret-proxy-pass".to_string();
    let masked = masked_proxy_password(Some(&secret)).unwrap();
    assert_eq!(masked, PROXY_PASSWORD_PLACEHOLDER, "读接口只回固定占位符");
    assert_eq!(
        masked_proxy_password(Some("pass")).as_deref(),
        Some("********")
    );

    // 原样回传读接口给的占位符：保留原密码。
    let mut current = Some(secret.clone());
    apply_proxy_password_update(&mut current, Some(masked.clone()));
    assert_eq!(current.as_deref(), Some(secret.as_str()));

    // 真正改密码、清空密码照常生效。
    apply_proxy_password_update(&mut current, Some("new-pass".to_string()));
    assert_eq!(current.as_deref(), Some("new-pass"));
    apply_proxy_password_update(&mut current, None);
    assert_eq!(current, None);

    // 没有旧密码时提交占位符也不会把占位符存成密码。
    let mut empty = None;
    apply_proxy_password_update(&mut empty, Some(masked));
    assert_eq!(empty, None);
}

#[test]
fn runtime_config_masks_proxy_password_and_rejects_bad_proxy_urls() {
    assert_eq!(masked_proxy_password(None), None);
    assert_eq!(masked_proxy_password(Some("")), None);
    let masked = masked_proxy_password(Some("super-secret-proxy-pass")).unwrap();
    assert!(!masked.contains("secret"), "{masked}");
    assert_eq!(masked, PROXY_PASSWORD_PLACEHOLDER);

    for valid in [
        None,
        Some("http://127.0.0.1:7890"),
        Some("https://proxy.example:8443"),
        Some("socks5://user:pass@10.0.0.1:1080"),
        Some("socks5h://proxy.example:1080"),
        Some("   "),
    ] {
        validate_proxy_url_field("proxyUrl", valid).expect("合法代理地址必须被接受");
        let mut config = Config::default();
        config.proxy_url = valid.map(str::to_string);
        normalize_and_validate_runtime_config(&mut config).expect("全局代理校验必须放行合法地址");
    }
    for invalid in [
        "127.0.0.1:7890",
        "ftp://proxy.example",
        "http://",
        "not a url",
    ] {
        let err = validate_proxy_url_field("proxyUrl", Some(invalid)).unwrap_err();
        assert!(err.to_string().contains("proxyUrl"), "{invalid}: {err}");
        if invalid == "127.0.0.1:7890" {
            // 全局代理不带协议时按 http 补全（与底层 HTTP 库一致），存量配置不会卡住保存。
            continue;
        }
        let mut config = Config::default();
        config.proxy_url = Some(invalid.to_string());
        assert!(
            normalize_and_validate_runtime_config(&mut config).is_err(),
            "全局代理 {invalid} 必须被拒绝"
        );
    }
    let mut config = Config::default();
    config.proxy_url = Some(" 127.0.0.1:7890 ".to_string());
    normalize_and_validate_runtime_config(&mut config).expect("不带协议的全局代理按 http 补全");
    assert_eq!(config.proxy_url.as_deref(), Some("http://127.0.0.1:7890"));
    let mut config = Config::default();
    config.proxy_url = Some("   ".to_string());
    normalize_and_validate_runtime_config(&mut config).expect("空白代理视为未配置");
    assert_eq!(config.proxy_url, None);
}

#[test]
fn runtime_config_audit_masks_proxy_url_credentials() {
    let mut changes = Vec::new();
    AdminService::append_runtime_config_changes(
        "proxyUrl",
        &serde_json::json!("http://alice:old-secret@proxy.example:3128"),
        &serde_json::json!("http://alice:new-secret@proxy.example:3129"),
        &mut changes,
    );
    let text = serde_json::to_string(&changes).unwrap();
    assert!(!text.contains("secret"), "{text}");
    assert!(!text.contains("alice"), "{text}");
    assert!(text.contains("proxy.example:3129"), "{text}");
    assert_eq!(
        mask_proxy_url_credentials("http://proxy.example:3128"),
        "http://proxy.example:3128"
    );
    assert!(proxy_url_has_credentials("socks5://u:p@10.0.0.1:1080"));
    assert!(!proxy_url_has_credentials("socks5://10.0.0.1:1080"));
}

#[test]
fn masked_account_proxy_credentials_are_restored_from_stored_value() {
    let stored = Some("http://alice:p%40ss@proxy.example:3128");
    assert!(proxy_url_has_masked_credentials(
        "http://***:***@proxy.example:3128/"
    ));
    assert!(proxy_url_has_masked_credentials(
        "http://alice:***@proxy.example:3128"
    ));
    assert!(!proxy_url_has_masked_credentials(
        "http://alice:new@proxy.example:3128"
    ));
    assert!(!proxy_url_has_masked_credentials(
        "http://proxy.example:3128"
    ));

    assert_eq!(
        restore_masked_proxy_credentials("http://***:***@proxy.example:3128/", stored).unwrap(),
        "http://alice:p%40ss@proxy.example:3128/"
    );
    // 换了端口但凭据保持打码：沿用原凭据，新端口生效。
    assert_eq!(
        restore_masked_proxy_credentials("http://***:***@proxy.example:3129/", stored).unwrap(),
        "http://alice:p%40ss@proxy.example:3129/"
    );
    // 只改了用户名：密码沿用。
    assert_eq!(
        restore_masked_proxy_credentials("http://bob:***@proxy.example:3128/", stored).unwrap(),
        "http://bob:p%40ss@proxy.example:3128/"
    );
    let err =
        restore_masked_proxy_credentials("http://***:***@proxy.example:3128/", None).unwrap_err();
    assert!(err.to_string().contains("没有可沿用"), "{err}");
    let err = restore_masked_proxy_credentials(
        "http://***:***@proxy.example:3128/",
        Some("http://proxy.example:3128"),
    )
    .unwrap_err();
    assert!(err.to_string().contains("没有可沿用"), "{err}");
}

#[test]
fn runtime_config_rejects_out_of_range_usage_record_limit() {
    for (limit, ok) in [
        (0, false),
        (1, true),
        (5_000, true),
        (100_000, true),
        (100_001, false),
    ] {
        let mut config = Config::default();
        config.usage_record_limit = limit;
        assert_eq!(
            normalize_and_validate_runtime_config(&mut config).is_ok(),
            ok,
            "usageRecordLimit {limit}"
        );
    }
}

#[test]
fn account_request_validation_rejects_blank_key_and_out_of_range_concurrency() {
    validate_account_api_key(None).expect("None 表示不改密钥");
    validate_account_api_key(Some("sk-ok")).expect("非空密钥合法");
    for blank in ["", "   ", "\t"] {
        let err = validate_account_api_key(Some(blank)).unwrap_err();
        assert!(matches!(err, AdminServiceError::InvalidRequest(_)));
        assert!(err.to_string().contains("账号密钥不能为空"));
    }
    validate_account_max_concurrent_requests(None).unwrap();
    validate_account_max_concurrent_requests(Some(1)).unwrap();
    validate_account_max_concurrent_requests(Some(MAX_ACCOUNT_CONCURRENT_REQUESTS)).unwrap();
    for invalid in [0, MAX_ACCOUNT_CONCURRENT_REQUESTS + 1, u32::MAX] {
        let err = validate_account_max_concurrent_requests(Some(invalid)).unwrap_err();
        assert!(matches!(err, AdminServiceError::InvalidRequest(_)));
        assert!(err.to_string().contains("maxConcurrentRequests"), "{err}");
    }
}

#[test]
fn account_usage_projection_bounds_are_already_enforced() {
    // 四个字段：三个 u32 已有 ≤200 上限，min_tokens 是 i32 已有 ≥0 下限；这里只确认不回退。
    for config in [
        AccountsConfig {
            usage_projection_uplift_percent: 201,
            ..AccountsConfig::default()
        },
        AccountsConfig {
            usage_projection_output_uplift_percent: 201,
            ..AccountsConfig::default()
        },
        AccountsConfig {
            usage_projection_output_uplift_min_tokens: -1,
            ..AccountsConfig::default()
        },
    ] {
        assert!(validate_accounts_config(&config).is_err());
    }
}

#[test]
fn probe_account_keeps_saved_transport_settings_and_defaults_for_unsaved() {
    let mut saved = crate::account::tests::test_pool("https://old.example/v1", true);
    saved.proxy_url = Some("socks5://10.0.0.1:1080".into());
    saved.header_profile = crate::account::AccountHeaderProfile::ClaudeCodeMimic;
    saved.tls_profile = crate::account::AccountTlsProfile::NativeTls;
    saved
        .header_overrides
        .insert("x-tenant".into(), "team".into());
    let probe = probe_account(
        Some(&saved),
        "https://new.example/v1".into(),
        "sk-new".into(),
        AccountAuthType::XApiKey,
    );
    assert_eq!(probe.base_url, "https://new.example/v1");
    assert_eq!(probe.api_key.as_deref(), Some("sk-new"));
    assert_eq!(probe.auth_type, AccountAuthType::XApiKey);
    assert_eq!(probe.proxy_url.as_deref(), Some("socks5://10.0.0.1:1080"));
    assert_eq!(
        probe.header_profile,
        crate::account::AccountHeaderProfile::ClaudeCodeMimic
    );
    assert_eq!(
        probe.tls_profile,
        crate::account::AccountTlsProfile::NativeTls
    );
    assert_eq!(
        probe.header_overrides.get("x-tenant").map(String::as_str),
        Some("team")
    );

    let fresh = probe_account(
        None,
        "https://fresh.example".into(),
        "sk-fresh".into(),
        AccountAuthType::Bearer,
    );
    assert_eq!(fresh.id, 0);
    assert_eq!(fresh.proxy_url, None);
    assert_eq!(fresh.header_profile, Default::default());
    assert_eq!(fresh.api_key.as_deref(), Some("sk-fresh"));
}

#[test]
fn accounts_config_with_transport_proxy_copies_global_proxy_settings() {
    let mut config = Config::default();
    config.proxy_url = Some("http://127.0.0.1:7890".into());
    config.proxy_username = Some("alice".into());
    config.proxy_password = Some("secret".into());
    let accounts = accounts_config_with_transport_proxy(&config);
    assert_eq!(
        accounts.transport_proxy_url.as_deref(),
        Some("http://127.0.0.1:7890")
    );
    assert_eq!(accounts.transport_proxy_username.as_deref(), Some("alice"));
    assert_eq!(accounts.transport_proxy_password.as_deref(), Some("secret"));
}

/// 处理器级审计测试：真实 PG/Redis，覆盖失败留痕、认证失败、采集与导出审计、模型提升审计。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_account_keeps_real_proxy_credentials_when_form_submits_masked_url() {
    let Some(postgres_url) = crate::storage::integration_test_url("CLAUDE_POOL_TEST_POSTGRES_URL")
    else {
        eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
        return;
    };
    let Some(redis_url) = crate::storage::integration_test_url("CLAUDE_POOL_TEST_REDIS_URL") else {
        eprintln!("跳过 Redis 集成测试：未设置 CLAUDE_POOL_TEST_REDIS_URL");
        return;
    };
    let mut config = Config::default();
    config.postgres.url = Some(postgres_url);
    config.postgres.max_connections = 3;
    config.redis.url = Some(redis_url);
    config.redis.key_prefix = format!("claude_pool:test:proxy_mask:{}", uuid::Uuid::new_v4());
    let postgres = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
    let redis = Arc::new(
        crate::storage::redis_cache::RedisStore::connect(&config)
            .await
            .unwrap(),
    );
    let service = AdminService::new(AdminServiceDependencies {
        runtime: crate::model::runtime::RuntimeConfigStore::new(Config::default(), None),
        usage_recorder: Arc::new(UsageRecorder::new(10)),
        pricing_catalog: Arc::new(PricingCatalog::new()),
        model_capabilities: Arc::new(ModelCapabilitiesCatalog::new()),
        postgres_store: postgres.clone(),
        observability_redis_store: None,
        request_api_key_store: Arc::new(RequestApiKeyStore::new(["sk-primary-12345678"])),
        request_admission: Arc::new(RequestAdmissionController::new(
            RequestAdmissionConfig::disabled(),
        )),
        account_manager: Arc::new(AccountManager::new(postgres.clone(), redis)),
    });
    let marker = uuid::Uuid::new_v4().simple().to_string();
    let create: CreateAccountRequest = serde_json::from_value(json!({
        "name": format!("proxy-mask-{marker}"),
        "baseUrl": "https://example.com",
        "apiKey": "sk-proxy-mask-12345678",
        "proxyUrl": "http://alice:real-secret@127.0.0.1:18080",
    }))
    .unwrap();

    let err = service
        .create_account(CreateAccountRequest {
            proxy_url: Some("http://***:***@127.0.0.1:18080/".into()),
            ..create.clone()
        })
        .unwrap_err();
    assert!(err.to_string().contains("打码占位"), "{err}");

    let created = service.create_account(create).unwrap();
    let masked = created.proxy_url.clone().unwrap();
    assert_eq!(masked, "http://***:***@127.0.0.1:18080/");
    let stored_proxy = || {
        let postgres = postgres.clone();
        let id = created.id;
        block_on_admin_store(async move { postgres.get_account(id, false).await })
            .unwrap()
            .unwrap()
            .proxy_url
    };

    // 表单原样提交打码地址：真实凭据保留。
    service
        .update_account(
            created.id,
            UpdateAccountRequest {
                name: Some(format!("proxy-mask-renamed-{marker}")),
                proxy_url: Some(Some(masked.clone())),
                ..UpdateAccountRequest::default()
            },
        )
        .unwrap();
    assert_eq!(
        stored_proxy().as_deref(),
        Some("http://alice:real-secret@127.0.0.1:18080/")
    );

    // 只改端口：凭据沿用，端口生效。
    service
        .update_account(
            created.id,
            UpdateAccountRequest {
                proxy_url: Some(Some("http://***:***@127.0.0.1:18081/".into())),
                ..UpdateAccountRequest::default()
            },
        )
        .unwrap();
    assert_eq!(
        stored_proxy().as_deref(),
        Some("http://alice:real-secret@127.0.0.1:18081/")
    );

    // 整段重填：用新凭据。
    service
        .update_account(
            created.id,
            UpdateAccountRequest {
                proxy_url: Some(Some("http://bob:new-secret@127.0.0.1:18082".into())),
                ..UpdateAccountRequest::default()
            },
        )
        .unwrap();
    assert_eq!(
        stored_proxy().as_deref(),
        Some("http://bob:new-secret@127.0.0.1:18082")
    );

    // 清空代理后再提交打码地址：没有可沿用的凭据，拒绝且不改库。
    service
        .update_account(
            created.id,
            UpdateAccountRequest {
                proxy_url: Some(None),
                ..UpdateAccountRequest::default()
            },
        )
        .unwrap();
    let err = service
        .update_account(
            created.id,
            UpdateAccountRequest {
                proxy_url: Some(Some("http://***:***@127.0.0.1:18080/".into())),
                ..UpdateAccountRequest::default()
            },
        )
        .unwrap_err();
    assert!(err.to_string().contains("没有可沿用"), "{err}");
    assert_eq!(stored_proxy(), None);

    service.delete_account(created.id).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_audit_records_failed_changes_auth_failures_capture_and_export() {
    use crate::admin::middleware::AdminState;
    use crate::storage::postgres::AdminAuditLogFilter;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    let Some(postgres_url) = crate::storage::integration_test_url("CLAUDE_POOL_TEST_POSTGRES_URL")
    else {
        eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
        return;
    };
    let Some(redis_url) = crate::storage::integration_test_url("CLAUDE_POOL_TEST_REDIS_URL") else {
        eprintln!("跳过 Redis 集成测试：未设置 CLAUDE_POOL_TEST_REDIS_URL");
        return;
    };
    let mut config = Config::default();
    config.postgres.url = Some(postgres_url);
    config.postgres.max_connections = 3;
    config.redis.url = Some(redis_url);
    config.redis.key_prefix = format!("claude_pool:test:admin_audit:{}", uuid::Uuid::new_v4());
    let postgres = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
    let redis = Arc::new(
        crate::storage::redis_cache::RedisStore::connect(&config)
            .await
            .unwrap(),
    );
    let runtime = crate::model::runtime::RuntimeConfigStore::new(Config::default(), None);
    let request_admission = Arc::new(RequestAdmissionController::new(
        RequestAdmissionConfig::disabled(),
    ));
    let service = AdminService::new(AdminServiceDependencies {
        runtime: runtime.clone(),
        usage_recorder: Arc::new(UsageRecorder::new(10)),
        pricing_catalog: Arc::new(PricingCatalog::new()),
        model_capabilities: Arc::new(ModelCapabilitiesCatalog::new()),
        postgres_store: postgres.clone(),
        observability_redis_store: None,
        request_api_key_store: Arc::new(RequestApiKeyStore::new(["sk-primary-12345678"])),
        request_admission,
        account_manager: Arc::new(AccountManager::new(postgres.clone(), redis)),
    });
    let since = Utc::now() - ChronoDuration::seconds(5);
    let marker = uuid::Uuid::new_v4().simple().to_string();

    // 准备：一条自定义路由和一个引用它的请求 Key，用于删路由检查（内置路由本来就不可删）。
    let team_prefix = format!("/dfcache/team-{marker}");
    runtime
        .update(|config| {
            config
                .routes
                .routes
                .push(crate::model::routes::RouteConfig::new(team_prefix.clone()));
            config.routes = config.routes.clone().assign_route_ids();
            config.set_request_api_keys(["sk-primary-12345678", "sk-team-a-12345678"]);
            config.set_request_api_key_policies(vec![RequestApiKeyPolicy {
                api_key: "sk-team-a-12345678".into(),
                name: format!("team-{marker}"),
                enabled: true,
                request_admission: None,
                usage_limits: Vec::new(),
                allowed_routes: vec![format!("{team_prefix}/")],
            }]);
        })
        .unwrap();

    // 失败变更：每一条都应落 success=false 的审计。
    let create = CreateAccountRequest {
        tags: Vec::new(),
        proxy_url: None,
        cost_multiplier: 1.0,
        overrides: Default::default(),
        name: format!("blank-key-{marker}"),
        base_url: "https://example.com/v1".into(),
        api_key: "   ".into(),
        auth_type: AccountAuthType::Bearer,
        header_profile: Default::default(),
        append_beta_query: false,
        header_overrides: Default::default(),
        wire_profile: Default::default(),
        tls_profile: Default::default(),
        enabled: true,
        priority: 100,
        max_concurrent_requests: 5,
        usage_projection_mode: Default::default(),
        stream_response_mode: None,
        auto_disable_policy: Default::default(),
        pre_output_stream_retry_mode: Default::default(),
        normalize_model_version_dots: false,
        model_mapping_mode: Default::default(),
        model_mapping_require_match: false,
        model_mapping_rules: Vec::new(),
        supported_models: Vec::new(),
        route_mode: Default::default(),
        route_rules: Vec::new(),
        notes: None,
    };
    let err = service.create_account(create.clone()).unwrap_err();
    assert!(err.to_string().contains("账号密钥不能为空"), "{err}");
    let err = service
        .create_account(CreateAccountRequest {
            api_key: "sk-valid-key".into(),
            max_concurrent_requests: 0,
            ..create.clone()
        })
        .unwrap_err();
    assert!(err.to_string().contains("maxConcurrentRequests"), "{err}");
    let err = service
        .create_account(CreateAccountRequest {
            api_key: "sk-valid-key".into(),
            proxy_url: Some("ftp://bad".into()),
            ..create.clone()
        })
        .unwrap_err();
    assert!(err.to_string().contains("proxyUrl"), "{err}");
    assert!(
        service
            .update_account(
                999_999_999,
                UpdateAccountRequest {
                    api_key: Some(String::new()),
                    ..UpdateAccountRequest::default()
                },
            )
            .is_err()
    );
    assert!(service.delete_account(999_999_999).is_err());
    assert!(
        service
            .update_runtime_config(UpdateRuntimeConfigRequest {
                proxy_url: Some(Some("not a url".into())),
                ..serde_json::from_value(json!({})).unwrap()
            })
            .is_err()
    );
    assert!(
        service
            .update_admin_api_key(UpdateAdminApiKeyRequest {
                admin_api_key: "short".into(),
            })
            .is_err()
    );
    assert!(
        service
            .create_request_api_key(
                CreateRequestApiKeyRequest {
                    api_key: Some("short".into()),
                    ..serde_json::from_value(json!({})).unwrap()
                },
                "sk-admin",
            )
            .is_err()
    );
    assert!(
        service
            .update_request_api_key(
                "missing-key-id",
                serde_json::from_value(json!({ "name": "x" })).unwrap(),
                "sk-admin",
            )
            .is_err()
    );
    assert!(
        service
            .delete_request_api_key("missing-key-id", "sk-admin")
            .is_err()
    );
    let mut routes = runtime.config().routes;
    routes.routes.retain(|route| route.prefix != team_prefix);
    let err = service.update_routes(routes.clone()).unwrap_err();
    assert!(err.to_string().contains(&format!("team-{marker}")), "{err}");
    assert!(err.to_string().contains(&team_prefix), "{err}");
    // 应用路由场景走同一个检查：场景快照里没有这条被引用的路由时拒绝应用，路由保持不变。
    let err = service.set_route_scenario_definitions(&routes).unwrap_err();
    assert!(err.to_string().contains(&format!("team-{marker}")), "{err}");
    assert!(
        runtime
            .config()
            .routes
            .routes
            .iter()
            .any(|route| route.prefix == team_prefix),
        "scenario apply must not delete a key-referenced route"
    );

    // 成功路径：创建账号 → 提升模型 → 删除，promote 要有审计。
    let created = service
        .create_account(CreateAccountRequest {
            api_key: "sk-valid-key".into(),
            supported_models: vec!["claude-sonnet-4-5".into()],
            ..create
        })
        .expect("合法账号必须能创建");
    service
        .promote_account_models(created.id)
        .await
        .expect("提升模型");
    service.delete_account(created.id).expect("删除测试账号");

    // 处理器层：认证失败、采集未初始化、带密钥导出。
    let state = AdminState::new("admin-secret-12345678", service);
    let router = crate::admin::create_admin_router(state);
    let source_ip = format!("203.0.113.{}", fastrand::u8(1..=254));
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/accounts")
                .header("x-api-key", "wrong-key")
                .header("x-forwarded-for", format!("{source_ip}, 10.0.0.1"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/diagnostics/capture/start")
                .header("x-api-key", "admin-secret-12345678")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/config/export?include_secrets=true")
                .header("x-api-key", "admin-secret-12345678")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // 审计是旁路异步写入，轮询等待落库。
    let expected_failures = [
        ("create_account", 3usize),
        ("update_account", 1),
        ("delete_account", 1),
        ("update_runtime_config", 1),
        ("update_admin_api_key", 1),
        ("create_request_api_key", 1),
        ("update_request_api_key", 1),
        ("delete_request_api_key", 1),
        ("update_routes", 1),
        ("start_capture", 1),
    ];
    let query = |action: &str, success: Option<bool>, actor: Option<&str>| {
        let filter = AdminAuditLogFilter {
            action: Some(action.to_string()),
            success,
            actor: actor.map(str::to_string),
            since: Some(since),
            ..AdminAuditLogFilter::default()
        };
        let postgres = postgres.clone();
        async move {
            postgres
                .query_admin_audit_logs_filtered(1, 200, &filter)
                .await
                .unwrap()
                .records
        }
    };
    for attempt in 0..100 {
        let mut all_ready = true;
        for (action, minimum) in expected_failures {
            if query(action, Some(false), None).await.len() < minimum {
                all_ready = false;
                break;
            }
        }
        if all_ready
            && !query("promote_account_models", Some(true), None)
                .await
                .is_empty()
            && !query("export_config", Some(true), None).await.is_empty()
            && !query("admin_auth_failed", Some(false), Some("anonymous"))
                .await
                .is_empty()
        {
            break;
        }
        assert!(attempt < 99, "审计日志在 10 秒内没有全部落库");
        tokio::time::sleep(StdDuration::from_millis(100)).await;
    }

    let rows = query("create_account", Some(false), None).await;
    let failures: Vec<&str> = rows
        .iter()
        .filter(|row| row.detail["name"] == json!(format!("blank-key-{marker}")))
        .filter_map(|row| row.error_message.as_deref())
        .collect();
    for expected in ["账号密钥不能为空", "maxConcurrentRequests", "proxyUrl"] {
        assert!(
            failures.iter().any(|message| message.contains(expected)),
            "创建失败审计缺少 {expected}: {failures:?}"
        );
    }
    assert!(
        rows.iter()
            .all(|row| !row.detail.to_string().contains("sk-valid-key")),
        "失败审计 detail 不能带密钥"
    );
    let routes_failure = query("update_routes", Some(false), None)
        .await
        .into_iter()
        .find(|row| {
            row.error_message
                .as_deref()
                .is_some_and(|message| message.contains(&format!("team-{marker}")))
        })
        .expect("删路由被 Key 引用阻止必须留痕");
    assert!(
        !routes_failure
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("sk-team-a")
    );
    let promote = query("promote_account_models", Some(true), None).await;
    assert!(promote.iter().any(
        |row| row.object_id.as_deref() == Some(&created.id.to_string())
            && row.object_type == "account"
    ));
    let export = query("export_config", Some(true), None).await;
    assert!(
        export
            .iter()
            .any(|row| row.object_type == "config" && row.detail["includeSecrets"] == json!(true))
    );
    let capture = query("start_capture", Some(false), None).await;
    assert!(capture.iter().any(|row| row.object_type == "capture"));
    let auth = query("admin_auth_failed", Some(false), Some("anonymous")).await;
    let auth = auth
        .iter()
        .find(|row| row.detail["sourceIp"] == json!(source_ip))
        .expect("认证失败审计必须带来源 IP");
    assert_eq!(auth.detail["path"], json!("/accounts"));
    assert_eq!(auth.object_type, "admin_auth");
    assert!(!auth.detail.to_string().contains("wrong-key"));
}
