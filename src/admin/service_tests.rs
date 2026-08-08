use super::*;
use crate::model::config::{ExternalPoolsConfig, RequestAdmissionConfig};
use std::collections::HashSet;

fn cleanup_request() -> UsageCleanupRequest {
    UsageCleanupRequest {
        mode: UsageCleanupMode::SoftDelete,
        older_than_days: None,
        cutoff_before: None,
        batch_size: None,
        max_batches: None,
        pause_ms_between_batches: None,
    }
}

#[test]
fn missing_auth_method_with_client_secret_import_is_inferred_as_idc() {
    let req: AddCredentialRequest = serde_json::from_value(serde_json::json!({
        "refreshToken": "fake-refresh-token",
        "accessToken": "fake-access-token",
        "clientId": "fake-client-id",
        "clientSecret": "fake-client-secret",
        "profileArn": "arn:aws:codewhisperer:us-east-1:123456789012:profile/FAKE"
    }))
    .unwrap();

    assert_eq!(resolve_add_credential_auth_method(&req), "idc");
}

#[test]
fn export_credentials_filter_keeps_selected_ids_and_rejects_missing_ids() {
    let mut credentials = vec![
        KiroCredentials {
            id: Some(1),
            ..Default::default()
        },
        KiroCredentials {
            id: Some(2),
            ..Default::default()
        },
        KiroCredentials {
            id: Some(3),
            ..Default::default()
        },
        KiroCredentials {
            id: None,
            ..Default::default()
        },
    ];
    filter_export_credentials_by_ids(&mut credentials, &HashSet::from([3, 1])).unwrap();
    assert_eq!(
        credentials
            .iter()
            .filter_map(|credential| credential.id)
            .collect::<Vec<_>>(),
        vec![1, 3]
    );

    let mut credentials = vec![KiroCredentials {
        id: Some(1),
        ..Default::default()
    }];
    let err = filter_export_credentials_by_ids(&mut credentials, &HashSet::from([1, 2]))
        .unwrap_err()
        .to_string();
    assert!(err.contains('2'));
}

#[test]
fn explicit_social_auth_method_is_preserved() {
    let req: AddCredentialRequest = serde_json::from_value(serde_json::json!({
        "refreshToken": "fake-refresh-token",
        "authMethod": "social",
        "clientId": "ignored-client-id",
        "clientSecret": "ignored-client-secret"
    }))
    .unwrap();

    assert_eq!(resolve_add_credential_auth_method(&req), "social");
}

#[test]
fn missing_auth_method_with_external_idp_fields_is_inferred_as_external_idp() {
    let req: AddCredentialRequest = serde_json::from_value(serde_json::json!({
        "refreshToken": "fake-refresh-token",
        "clientId": "fake-client-id",
        "issuerUrl": "https://login.microsoftonline.com/common/v2.0",
        "scopes": "offline_access"
    }))
    .unwrap();

    assert_eq!(resolve_add_credential_auth_method(&req), "external_idp");
}

#[test]
fn missing_auth_method_with_kiro_api_key_is_inferred_as_api_key() {
    let req: AddCredentialRequest = serde_json::from_value(serde_json::json!({
        "kiroApiKey": "ksk_test_key|eu-central-1"
    }))
    .unwrap();

    assert_eq!(resolve_add_credential_auth_method(&req), "api_key");
}

#[test]
fn discovered_supported_models_preserve_non_claude_model_ids() {
    let models = AdminService::normalize_discovered_supported_models(vec![
        " QWEN3-CODER-NEXT ".to_string(),
        "minimax-m2.5".to_string(),
        "qwen3-coder-next".to_string(),
    ]);

    assert_eq!(
        models,
        vec!["qwen3-coder-next".to_string(), "minimax-m2.5".to_string()]
    );
}

#[test]
fn runtime_cooldown_validation_allows_base_values_above_max_cap() {
    validate_runtime_cooldown_settings(10, 2, &[30, 5, 10, 60])
        .expect("max cooldown is an upper cap applied at runtime");
}

#[test]
fn runtime_cooldown_validation_rejects_zero_values() {
    assert!(matches!(
        validate_runtime_cooldown_settings(0, 2, &[1]),
        Err(AdminServiceError::InvalidCredential(_))
    ));
    assert!(matches!(
        validate_runtime_cooldown_settings(1, 0, &[1]),
        Err(AdminServiceError::InvalidCredential(_))
    ));
    assert!(matches!(
        validate_runtime_cooldown_settings(1, 2, &[1, 0]),
        Err(AdminServiceError::InvalidCredential(_))
    ));
}

#[test]
fn external_pool_transient_failure_priority_penalty_validation_is_bounded() {
    let mut config = ExternalPoolsConfig {
        external_pool_transient_failure_priority_penalty: 10_000,
        ..ExternalPoolsConfig::default()
    };
    validate_external_pools_config(&config).expect("upper bound should be accepted");

    config.external_pool_transient_failure_priority_penalty = 10_001;
    let err = validate_external_pools_config(&config).unwrap_err();
    assert!(
        err.contains("externalPoolTransientFailurePriorityPenalty"),
        "unexpected validation error: {err}"
    );
}

#[test]
fn external_pool_cost_floor_margin_validation_is_bounded() {
    let mut config = ExternalPoolsConfig {
        external_pool_usage_projection_cost_floor_margin_percent: 200,
        ..ExternalPoolsConfig::default()
    };
    validate_external_pools_config(&config).expect("upper bound should be accepted");

    config.external_pool_usage_projection_cost_floor_margin_percent = 201;
    let err = validate_external_pools_config(&config).unwrap_err();
    assert!(
        err.contains("externalPoolUsageProjectionCostFloorMarginPercent"),
        "unexpected validation error: {err}"
    );
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
fn credential_admin_list_options_include_account_info_snapshot() {
    let options = CredentialStatusBuildOptions::WITH_ACCOUNT_INFO_AND_COST_SUMMARY;

    assert!(options.include_account_info);
    assert!(options.include_cost_summary);
}

#[test]
fn credit_snapshot_uses_overage_bonus_for_all_paid_tiers() {
    let free_without_overage =
        credit_snapshot_for_subscription(Some("KIRO FREE"), 34.52, 50.0, 0.0);
    assert_eq!(free_without_overage.limit, 50.0);
    assert!((free_without_overage.remaining - 15.48).abs() < 1e-9);
    assert_eq!(free_without_overage.base, 50.0);
    assert_eq!(free_without_overage.bonus, 0.0);

    let pro_without_overage =
        credit_snapshot_for_subscription(Some("Kiro Pro"), 125.25, 1_000.0, 0.0);
    assert_eq!(pro_without_overage.limit, 1_000.0);
    assert_eq!(pro_without_overage.remaining, 874.75);
    assert_eq!(pro_without_overage.base, 1_000.0);
    assert_eq!(pro_without_overage.bonus, 0.0);

    let pro_with_overage =
        credit_snapshot_for_subscription(Some("Kiro Pro"), 125.25, 11_000.0, 10_000.0);
    assert_eq!(pro_with_overage.limit, 11_000.0);
    assert_eq!(pro_with_overage.remaining, 10_874.75);
    assert_eq!(pro_with_overage.base, 1_000.0);
    assert_eq!(pro_with_overage.bonus, 10_000.0);

    let pro_plus_with_overage =
        credit_snapshot_for_subscription(Some("Kiro Pro+"), 125.25, 12_000.0, 10_000.0);
    assert_eq!(pro_plus_with_overage.limit, 12_000.0);
    assert_eq!(pro_plus_with_overage.remaining, 11_874.75);
    assert_eq!(pro_plus_with_overage.base, 2_000.0);
    assert_eq!(pro_plus_with_overage.bonus, 10_000.0);

    let pro_max_with_overage =
        credit_snapshot_for_subscription(Some("Kiro Pro Max"), 125.25, 15_000.0, 10_000.0);
    assert_eq!(pro_max_with_overage.limit, 15_000.0);
    assert_eq!(pro_max_with_overage.remaining, 14_874.75);
    assert_eq!(pro_max_with_overage.base, 5_000.0);
    assert_eq!(pro_max_with_overage.bonus, 10_000.0);

    let power_without_overage =
        credit_snapshot_for_subscription(Some("Kiro Power"), 125.25, 10_000.0, 0.0);
    assert_eq!(power_without_overage.limit, 10_000.0);
    assert_eq!(power_without_overage.remaining, 9_874.75);
    assert_eq!(power_without_overage.base, 10_000.0);
    assert_eq!(power_without_overage.bonus, 0.0);

    let power_with_overage =
        credit_snapshot_for_subscription(Some("Kiro Power"), 125.25, 20_000.0, 10_000.0);
    assert_eq!(power_with_overage.limit, 20_000.0);
    assert_eq!(power_with_overage.remaining, 19_874.75);
    assert_eq!(power_with_overage.base, 10_000.0);
    assert_eq!(power_with_overage.bonus, 10_000.0);
}

#[test]
fn subscription_key_and_rank_distinguish_pro_max_from_pro() {
    for title in ["Kiro Pro Max", "KIRO PRO_MAX", "pro-max", "promax"] {
        assert_eq!(subscription_key(Some(title)), "pro_max", "title={title}");
        assert_eq!(subscription_rank(Some(title)), 5, "title={title}");
    }
    assert_eq!(subscription_key(Some("Kiro Pro")), "pro");
    assert_eq!(subscription_rank(Some("Kiro Pro")), 3);
    assert_eq!(subscription_key(Some("Kiro Pro+")), "pro_plus");
    assert_eq!(subscription_rank(Some("Kiro Pro+")), 4);
    assert_eq!(subscription_key(Some("Kiro Power")), "power");
    assert_eq!(subscription_rank(Some("Kiro Power")), 6);
}

#[test]
fn live_credit_snapshot_does_not_infer_bonus_from_usage_limit() {
    let pro_without_active_bonus =
        credit_snapshot_for_subscription(Some("Kiro Pro"), 125.25, 11_000.0, 0.0);

    assert_eq!(pro_without_active_bonus.limit, 1_000.0);
    assert_eq!(pro_without_active_bonus.remaining, 874.75);
    assert_eq!(pro_without_active_bonus.base, 1_000.0);
    assert_eq!(pro_without_active_bonus.bonus, 0.0);
}

#[test]
fn persisted_credit_snapshot_recomputes_from_usage_limit() {
    let old_wrong_power = credit_snapshot_from_persisted_fields(
        Some("Kiro Power"),
        250.0,
        20_000.0,
        10_000.0,
        10_000.0,
        9_750.0,
        10_000.0,
    );
    assert_eq!(old_wrong_power.limit, 20_000.0);
    assert_eq!(old_wrong_power.remaining, 19_750.0);
    assert_eq!(old_wrong_power.base, 10_000.0);
    assert_eq!(old_wrong_power.bonus, 10_000.0);

    let old_wrong_pro_without_overage = credit_snapshot_from_persisted_fields(
        Some("Kiro Pro"),
        250.0,
        1_000.0,
        11_000.0,
        10_750.0,
        1_000.0,
        10_000.0,
    );
    assert_eq!(old_wrong_pro_without_overage.limit, 1_000.0);
    assert_eq!(old_wrong_pro_without_overage.remaining, 750.0);
    assert_eq!(old_wrong_pro_without_overage.base, 1_000.0);
    assert_eq!(old_wrong_pro_without_overage.bonus, 0.0);
}

#[test]
fn persisted_credit_snapshot_only_infers_fixed_overage_bonus() {
    let trial_like_extra_limit = credit_snapshot_from_persisted_fields(
        Some("Kiro Pro"),
        125.0,
        1_500.0,
        11_000.0,
        10_875.0,
        1_000.0,
        10_000.0,
    );

    assert_eq!(trial_like_extra_limit.limit, 1_000.0);
    assert_eq!(trial_like_extra_limit.remaining, 875.0);
    assert_eq!(trial_like_extra_limit.base, 1_000.0);
    assert_eq!(trial_like_extra_limit.bonus, 0.0);
}

fn credential_item(
    id: u64,
    disabled: bool,
    created_at: Option<&str>,
    success_count: u64,
    estimated_cost_usd: f64,
    usage_percentage: Option<f64>,
) -> CredentialStatusItem {
    CredentialStatusItem {
        id,
        created_at: created_at.map(str::to_string),
        updated_at: None,
        priority: id as u32,
        disabled,
        failure_count: 0,
        is_current: false,
        expires_at: None,
        auth_method: None,
        provider: None,
        region: None,
        auth_region: None,
        api_region: None,
        effective_auth_region: "us-east-1".to_string(),
        effective_api_region: "us-east-1".to_string(),
        has_profile_arn: false,
        refresh_token_hash: None,
        api_key_hash: None,
        masked_api_key: None,
        email: Some(format!("user{}@example.com", id)),
        subscription_title: None,
        account_info: usage_percentage.map(|usage_percentage| CredentialAccountInfo {
            subscription_title: Some("Kiro Pro".to_string()),
            current_usage: usage_percentage,
            usage_limit: 100.0,
            remaining: 100.0 - usage_percentage,
            usage_percentage,
            credit_limit: 11_000.0,
            credit_remaining: (11_000.0 - usage_percentage).max(0.0),
            credit_base: 1_000.0,
            credit_bonus: 10_000.0,
            overage_status: Some("ENABLED".to_string()),
            overage_capability: Some("OVERAGE_CAPABLE".to_string()),
            overage_cap: 10.0,
            overage_rate: 0.04,
            current_overages: 0.0,
            next_reset_at: None,
            checked_at: "2026-01-01T00:00:00Z".to_string(),
        }),
        success_count,
        last_used_at: None,
        supported_models: Vec::new(),
        has_proxy: false,
        proxy_url: None,
        proxy_username: None,
        proxy_password: None,
        proxy_resource_id: None,
        proxy_resource_name: None,
        effective_proxy_url: None,
        effective_proxy_source: "none".to_string(),
        refresh_failure_count: 0,
        disabled_reason: None,
        endpoint: "ide".to_string(),
        cooled_down: false,
        cooldown_remaining_secs: 0,
        cooldown_reason: None,
        rate_limited: false,
        rate_limit_remaining_secs: 0,
        in_flight_requests: 0,
        oldest_in_flight_age_secs: 0,
        newest_in_flight_idle_secs: 0,
        max_concurrent_requests: 0,
        max_concurrent_requests_override: None,
        rpm: 0,
        rpm_override: None,
        rate_limit_auto_disable_enabled: true,
        in_flight_lease_max_secs: 0,
        warmup_remaining: 0,
        transient_failure_streak: 0,
        recent_error_rate: 0.0,
        latency_ewma_ms: None,
        last_error_kind: None,
        last_error_reason: None,
        last_error_at_ms: None,
        in_probation: false,
        probation_remaining_secs: 0,
        scheduler_selection_count: 0,
        recent_scheduler_selection_count_10s: 0,
        recent_scheduler_selection_count_60s: 0,
        recent_scheduler_selection_count_5m: 0,
        scheduler_selection_pressure: 0.0,
        scheduler_score: 0.0,
        estimated_cost_usd,
        kiro_metering_usage: 0.0,
        priced_requests: 0,
        unpriced_requests: 0,
    }
}

#[test]
fn credential_default_sort_keeps_enabled_then_newest_created() {
    let mut credentials = vec![
        credential_item(1, false, Some("2026-01-01T00:00:00Z"), 0, 0.0, None),
        credential_item(2, true, Some("2026-01-03T00:00:00Z"), 0, 0.0, None),
        credential_item(3, false, Some("2026-01-02T00:00:00Z"), 0, 0.0, None),
        credential_item(4, false, None, 0, 0.0, None),
    ];

    sort_credentials_for_admin_display(&mut credentials);

    let ids: Vec<u64> = credentials
        .into_iter()
        .map(|credential| credential.id)
        .collect();
    assert_eq!(ids, vec![3, 1, 4, 2]);
}

#[test]
fn credential_custom_sort_runs_before_pagination_order() {
    let mut credentials = vec![
        credential_item(1, false, Some("2026-01-01T00:00:00Z"), 10, 0.2, Some(20.0)),
        credential_item(2, false, Some("2026-01-02T00:00:00Z"), 30, 0.1, Some(90.0)),
        credential_item(3, true, Some("2026-01-03T00:00:00Z"), 20, 0.3, None),
    ];
    let query = CredentialListQuery {
        sort_by: Some("success_count".to_string()),
        sort_order: Some("desc".to_string()),
        ..Default::default()
    };

    sort_credentials_for_admin_display_with_query(&mut credentials, &query);

    let ids: Vec<u64> = credentials
        .into_iter()
        .map(|credential| credential.id)
        .collect();
    assert_eq!(ids, vec![2, 3, 1]);
}

#[test]
fn credential_filters_and_search_match_scheduling_overrides() {
    let mut credential = credential_item(9, false, Some("2026-01-01T00:00:00Z"), 0, 0.0, None);
    credential.priority = 4;
    credential.max_concurrent_requests = 7;
    credential.max_concurrent_requests_override = Some(7);
    credential.rpm = 60;
    credential.rpm_override = Some(60);
    credential.api_region = Some("us-west-2".to_string());
    credential.effective_api_region = "us-west-2".to_string();
    credential.supported_models = vec!["claude-opus-4.8".to_string()];

    for status in [
        "custom_priority",
        "custom_concurrency",
        "custom_rpm",
        "custom_scheduling",
    ] {
        let query = CredentialListQuery {
            status: Some(status.to_string()),
            ..Default::default()
        };
        assert!(
            credential_matches_query(&credential, &query),
            "{status} should match"
        );
    }

    for q in ["#9", "id:9", "priority:4", "concurrency:7", "rpm:60"] {
        let query = CredentialListQuery {
            q: Some(q.to_string()),
            ..Default::default()
        };
        assert!(
            credential_matches_query(&credential, &query),
            "{q} should match"
        );
    }

    for query in [
        CredentialListQuery {
            credential_id: Some(9),
            ..Default::default()
        },
        CredentialListQuery {
            account: Some("user9@example.com".to_string()),
            ..Default::default()
        },
        CredentialListQuery {
            region: Some("us-west-2".to_string()),
            ..Default::default()
        },
        CredentialListQuery {
            model: Some("opus-4.8".to_string()),
            ..Default::default()
        },
        CredentialListQuery {
            endpoint: Some("ide".to_string()),
            ..Default::default()
        },
        CredentialListQuery {
            priority: Some(4),
            rpm: Some(60),
            concurrency: Some(7),
            ..Default::default()
        },
    ] {
        assert!(credential_matches_query(&credential, &query));
    }
}

#[test]
fn usage_cleanup_request_uses_safe_manual_defaults() {
    let before = Utc::now();
    let plan = normalize_usage_cleanup_request(cleanup_request()).expect("valid request");
    let after = Utc::now();

    assert_eq!(plan.mode, UsageCleanupMode::SoftDelete);
    assert_eq!(plan.batch_size, USAGE_CLEANUP_DEFAULT_BATCH_SIZE);
    assert_eq!(plan.max_batches, USAGE_CLEANUP_DEFAULT_MAX_BATCHES);
    assert_eq!(plan.pause_ms_between_batches, 100);
    assert!(plan.cutoff >= before - ChronoDuration::days(7) - ChronoDuration::seconds(1));
    assert!(plan.cutoff <= after - ChronoDuration::days(7) + ChronoDuration::seconds(1));
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
    request.pause_ms_between_batches = Some(0);

    let plan = normalize_usage_cleanup_request(request).expect("valid request");

    assert_eq!(plan.mode, UsageCleanupMode::HardDelete);
    assert_eq!(plan.cutoff, cutoff);
    assert_eq!(plan.batch_size, USAGE_CLEANUP_MAX_BATCH_SIZE);
    assert_eq!(plan.max_batches, 10_000);
    assert_eq!(plan.pause_ms_between_batches, 0);

    let mut above_legacy_limit = cleanup_request();
    above_legacy_limit.batch_size = Some(501);
    let plan =
        normalize_usage_cleanup_request(above_legacy_limit).expect("501 is within current limit");
    assert_eq!(plan.batch_size, 501);
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
}

#[test]
fn usage_cleanup_resume_gets_a_fresh_per_run_batch_budget() {
    assert!(usage_cleanup_run_batch_limit_reached(10, 0, 10));
    assert!(!usage_cleanup_run_batch_limit_reached(10, 10, 10));
    assert!(!usage_cleanup_run_batch_limit_reached(19, 10, 10));
    assert!(usage_cleanup_run_batch_limit_reached(20, 10, 10));
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

#[tokio::test]
async fn usage_cleanup_lease_renewal_recovers_from_transient_row_lock_for_three_rounds() {
    let Some(url) = crate::storage::integration_test_url("KIRO_RS_TEST_POSTGRES_URL") else {
        eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };
    let mut config = crate::model::config::Config::default();
    config.postgres.url = Some(url);
    config.postgres.max_connections = 2;
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
                cutoff_at: Utc::now() - ChronoDuration::days(7),
                batch_size: 250,
                max_batches: 100,
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
                    attempt_timeout: StdDuration::from_millis(40),
                    max_attempts: 3,
                    retry_delay: StdDuration::from_millis(15),
                },
            )
            .await
        });

        tokio::time::sleep(StdDuration::from_millis(80)).await;
        blocker.rollback().await.unwrap();
        let renewed = renewal.await.unwrap().unwrap();
        let elapsed = started.elapsed();
        assert_eq!(renewed, Some(false), "round {round}");
        assert!(
            elapsed >= StdDuration::from_millis(70),
            "round {round}: the injected row lock must affect at least one attempt: {elapsed:?}"
        );
        assert!(
            elapsed < StdDuration::from_millis(250),
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
fn admin_request_key_id_matches_runtime_authenticated_identity() {
    let items = request_api_key_items(&[" sk-one ".to_string()]);
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
fn extracts_model_ids_from_kiro_compatible_models_response() {
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
