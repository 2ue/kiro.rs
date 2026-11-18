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
