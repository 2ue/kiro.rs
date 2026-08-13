use super::{
    AccountAuthPolicy, AccountId, AccountLimits, AccountProxy, AccountSecretRef, UpstreamAccount,
};

pub fn upstream_account_from_external_pool(
    pool: &crate::external_pool::ExternalPool,
) -> Option<UpstreamAccount> {
    use crate::external_pool::ExternalPoolAuthType;

    let id = AccountId::new(pool.id)?;
    let secret = pool
        .api_key
        .as_ref()
        .map(|_| AccountSecretRef::InlineRedacted("present".to_string()))
        .or_else(|| {
            pool.masked_api_key
                .as_ref()
                .map(|masked| AccountSecretRef::InlineRedacted(masked.clone()))
        })
        .unwrap_or_else(|| AccountSecretRef::InlineRedacted("absent".to_string()));
    let auth = match pool.auth_type {
        ExternalPoolAuthType::Bearer => AccountAuthPolicy::Bearer { secret },
        ExternalPoolAuthType::XApiKey => AccountAuthPolicy::Header {
            name: "x-api-key".to_string(),
            secret,
        },
    };

    Some(UpstreamAccount {
        id,
        name: pool.name.clone(),
        enabled: pool.enabled && !pool.is_auto_disabled_now(),
        base_url: pool.base_url.clone(),
        auth,
        supported_models: pool.supported_models.clone(),
        limits: AccountLimits {
            priority: pool.priority.max(0) as u32,
            rpm: None,
            max_concurrent_requests: Some(pool.max_concurrent_requests),
        },
        proxy: AccountProxy::Inherit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        external_pool::{
            ExternalPool, ExternalPoolAuthType, ExternalPoolAutoDisablePolicy,
            ExternalPoolModelMappingMode, ExternalPoolRawModelMode, ExternalPoolRequestBodyMode,
            ExternalPoolStreamRetryMode, ExternalPoolUsageProjectionMode,
        },
        model::config::ExternalPoolRouteMode,
    };
    use chrono::Utc;

    #[test]
    fn external_pool_projects_to_upstream_account_boundary() {
        let pool = ExternalPool {
            id: 7,
            revision: 3,
            name: "account-primary".to_string(),
            base_url: "https://upstream.example.test".to_string(),
            api_key: Some("sk-sensitive".to_string()),
            masked_api_key: None,
            auth_type: ExternalPoolAuthType::XApiKey,
            enabled: true,
            priority: 12,
            max_concurrent_requests: 4,
            usage_projection_mode: ExternalPoolUsageProjectionMode::CurrentPathPolicy,
            stream_response_mode: None,
            request_body_mode: ExternalPoolRequestBodyMode::Normalized,
            raw_model_mode: ExternalPoolRawModelMode::None,
            auto_disable_policy: ExternalPoolAutoDisablePolicy::Inherit,
            pre_output_stream_retry_mode: ExternalPoolStreamRetryMode::Inherit,
            auto_disabled: false,
            auto_disabled_reason: None,
            auto_disabled_at: None,
            auto_disabled_until: None,
            auto_disabled_last_error: None,
            preserve_path: true,
            normalize_model_version_dots: false,
            model_mapping_mode: ExternalPoolModelMappingMode::ProcessedMapping,
            model_mapping_require_match: false,
            model_mapping_rules: Vec::new(),
            supported_models: vec!["claude-sonnet-*".to_string()],
            route_mode: ExternalPoolRouteMode::AllowAll,
            route_rules: Vec::new(),
            notes: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let account = upstream_account_from_external_pool(&pool)
            .expect("positive pool id should project to account id");
        assert_eq!(account.id.get(), 7);
        assert_eq!(account.name, "account-primary");
        assert_eq!(account.base_url, "https://upstream.example.test");
        assert!(account.enabled);
        assert_eq!(account.limits.priority, 12);
        assert_eq!(account.limits.max_concurrent_requests, Some(4));
        assert!(account.supports_model(Some("claude-sonnet-4-6")));
        assert!(!account.supports_model(Some("claude-opus-4-1")));
        assert!(matches!(
            account.auth,
            AccountAuthPolicy::Header { ref name, .. } if name == "x-api-key"
        ));
    }
}
