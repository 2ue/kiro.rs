use super::*;

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
    assert_eq!(plan.batch_size, 1000);
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
    request.batch_size = Some(5000);
    request.max_batches = Some(10_000);
    request.pause_ms_between_batches = Some(0);

    let plan = normalize_usage_cleanup_request(request).expect("valid request");

    assert_eq!(plan.mode, UsageCleanupMode::HardDelete);
    assert_eq!(plan.cutoff, cutoff);
    assert_eq!(plan.batch_size, 5000);
    assert_eq!(plan.max_batches, 10_000);
    assert_eq!(plan.pause_ms_between_batches, 0);
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
    large_batch.batch_size = Some(5001);
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
fn remove_request_api_key_by_id_removes_requested_key() {
    let mut keys = vec!["sk-one".to_string(), "sk-two".to_string()];

    let removed = remove_request_api_key_by_id(&mut keys, &request_api_key_id("sk-one"))
        .expect("key should be removed");

    assert_eq!(removed, "sk-one");
    assert_eq!(keys, vec!["sk-two".to_string()]);
}

#[test]
fn remove_request_api_key_by_id_rejects_missing_key() {
    let mut keys = vec!["sk-one".to_string(), "sk-two".to_string()];

    let err = remove_request_api_key_by_id(&mut keys, &request_api_key_id("sk-missing"))
        .expect_err("missing key should fail");

    assert!(matches!(err, AdminServiceError::InvalidCredential(_)));
    assert_eq!(keys, vec!["sk-one".to_string(), "sk-two".to_string()]);
}

#[test]
fn remove_request_api_key_by_id_rejects_removing_last_key() {
    let mut keys = vec!["sk-one".to_string()];

    let err = remove_request_api_key_by_id(&mut keys, &request_api_key_id("sk-one"))
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
