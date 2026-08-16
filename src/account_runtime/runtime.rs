//! Account runtime facade.
//!
//! The delegated implementation still lives behind a legacy scheduler module
//! while it is being migrated. New integration points should depend on this
//! account runtime boundary instead of importing the legacy scheduler manager
//! directly.

use std::time::Duration;

use axum::response::Response;

pub type AccountRuntimeManager = crate::external_pool::ExternalPoolManager;

pub type AccountRuntimeConfig = crate::model::config::ExternalPoolsConfig;

pub type AccountAuthType = crate::external_pool::ExternalPoolAuthType;

pub type AccountUsageProjectionMode = crate::external_pool::ExternalPoolUsageProjectionMode;

pub type AccountStreamResponseMode = crate::model::config::ExternalPoolStreamResponseMode;

pub type UpstreamAccountStorageRecord = crate::external_pool::ExternalPool;

pub type UpstreamAccountStatusRecord = crate::external_pool::ExternalPoolStatus;

pub(crate) type AccountRuntimeStatusCompatibilityResponse =
    crate::external_pool::ExternalPoolsStatusResponse;

pub type AccountRouteRequest = crate::external_pool::ExternalRouteRequest;

pub(crate) type AccountRouteRequestPreparationCache =
    crate::external_pool::ExternalRouteRequestPreparationCache;

pub type AccountForwardOutcome = crate::external_pool::ExternalPoolForwardOutcome;

pub type AccountFinalError = crate::external_pool::ExternalPoolFinalError;

pub type AccountRequestBodyMode = crate::external_pool::ExternalPoolRequestBodyMode;

pub type AccountRawModelMode = crate::external_pool::ExternalPoolRawModelMode;

pub type AccountAutoDisablePolicy = crate::external_pool::ExternalPoolAutoDisablePolicy;

pub type AccountStreamRetryMode = crate::external_pool::ExternalPoolStreamRetryMode;

pub type AccountModelMappingMode = crate::external_pool::ExternalPoolModelMappingMode;

pub type AccountRouteMode = crate::model::config::ExternalPoolRouteMode;

pub type AccountLatencyTraceState = crate::external_pool::ExternalLatencyTraceState;

pub(crate) type UpstreamAccountEligibility = crate::external_pool::ExternalPoolEligibility;

pub(crate) fn mask_upstream_account_key(value: &str) -> String {
    crate::external_pool::mask_external_pool_key(value)
}

pub(crate) fn normalize_upstream_account_model_mapping_rules(
    rules: Vec<crate::model::config::ModelMappingRule>,
) -> Vec<crate::model::config::ModelMappingRule> {
    crate::external_pool::normalize_external_pool_model_mapping_rules(rules)
}

pub async fn load_upstream_account_status_records(
    manager: &AccountRuntimeManager,
    config: &AccountRuntimeConfig,
) -> anyhow::Result<Vec<UpstreamAccountStatusRecord>> {
    manager.status(config).await
}

pub async fn clear_upstream_account_cooldowns(
    manager: &AccountRuntimeManager,
    account_id: u64,
) -> anyhow::Result<usize> {
    manager.clear_pool_cooldowns(account_id).await
}

pub fn cached_eligible_account_for_route_and_model(
    manager: &AccountRuntimeManager,
    config: &AccountRuntimeConfig,
    endpoint: &str,
    model: &str,
) -> bool {
    manager.has_cached_eligible_pool_for_route_and_model(config, endpoint, model)
}

pub async fn eligible_account_for_route_and_model(
    manager: &AccountRuntimeManager,
    config: &AccountRuntimeConfig,
    endpoint: &str,
    model: &str,
) -> bool {
    manager
        .has_eligible_pool_for_route_and_model(config, endpoint, model)
        .await
}

pub fn cached_eligible_account_for_route_body_mode_and_model(
    manager: &AccountRuntimeManager,
    config: &AccountRuntimeConfig,
    endpoint: &str,
    body_mode: AccountRequestBodyMode,
    model: &str,
) -> bool {
    manager
        .has_cached_eligible_pool_for_route_body_mode_and_model(config, endpoint, body_mode, model)
}

pub async fn eligible_account_for_route_body_mode_and_model(
    manager: &AccountRuntimeManager,
    config: &AccountRuntimeConfig,
    endpoint: &str,
    body_mode: AccountRequestBodyMode,
    model: &str,
) -> bool {
    manager
        .has_eligible_pool_for_route_body_mode_and_model(config, endpoint, body_mode, model)
        .await
}

pub fn cached_immediately_available_account_for_route_and_model(
    manager: &AccountRuntimeManager,
    config: &AccountRuntimeConfig,
    endpoint: &str,
    model: &str,
) -> bool {
    manager.has_cached_immediately_available_pool_for_route_and_model(config, endpoint, model)
}

pub async fn immediately_available_account_for_route_and_model(
    manager: &AccountRuntimeManager,
    config: &AccountRuntimeConfig,
    endpoint: &str,
    model: &str,
    max_wait: Duration,
) -> bool {
    manager
        .has_immediately_available_pool_for_route_and_model(config, endpoint, model, max_wait)
        .await
}

pub fn cached_immediately_available_account_for_route_body_mode_and_model(
    manager: &AccountRuntimeManager,
    config: &AccountRuntimeConfig,
    endpoint: &str,
    body_mode: AccountRequestBodyMode,
    model: &str,
) -> bool {
    manager.has_cached_immediately_available_pool_for_route_body_mode_and_model(
        config, endpoint, body_mode, model,
    )
}

pub async fn immediately_available_account_for_route_body_mode_and_model(
    manager: &AccountRuntimeManager,
    config: &AccountRuntimeConfig,
    endpoint: &str,
    body_mode: AccountRequestBodyMode,
    model: &str,
    max_wait: Duration,
) -> bool {
    manager
        .has_immediately_available_pool_for_route_body_mode_and_model(
            config, endpoint, body_mode, model, max_wait,
        )
        .await
}

pub async fn account_direct_policy_reason(
    manager: &AccountRuntimeManager,
    config: &AccountRuntimeConfig,
    endpoint: &str,
    model: &str,
) -> Option<String> {
    manager.direct_policy_reason(config, endpoint, model).await
}

pub async fn forward_account_with_failover(
    manager: &AccountRuntimeManager,
    config: AccountRuntimeConfig,
    route: AccountRouteRequest,
) -> Response {
    manager.forward_with_failover(config, route).await
}

pub async fn forward_account_with_failover_result(
    manager: &AccountRuntimeManager,
    config: AccountRuntimeConfig,
    route: AccountRouteRequest,
) -> AccountForwardOutcome {
    manager.forward_with_failover_result(config, route).await
}

pub fn upstream_account_models_url(base_url: &str) -> Result<reqwest::Url, url::ParseError> {
    crate::external_pool::external_pool_models_url(base_url)
}

pub fn upstream_account_messages_url(base_url: &str) -> Result<reqwest::Url, url::ParseError> {
    crate::external_pool::external_pool_messages_url(base_url)
}

pub trait AccountRuntimeConfigExt {
    fn account_runtime_enabled(&self) -> bool;
    fn account_route_allowed(&self, endpoint: &str) -> bool;

    fn account_runtime_enabled_for_endpoint(&self, endpoint: &str) -> bool {
        self.account_runtime_enabled() && self.account_route_allowed(endpoint)
    }

    fn effective_account_dispatch_max_wait_secs(&self) -> u64;
    fn account_direct_policy_enabled(&self) -> bool;
    fn account_local_rescue_enabled(&self) -> bool;
    fn account_local_rescue_on_rate_limit(&self) -> bool;
    fn account_local_rescue_on_timeout(&self) -> bool;
    fn account_local_rescue_on_capacity(&self) -> bool;
    fn account_local_rescue_max_wait_secs(&self) -> u64;
    fn clamped_account_request_timeout_secs(&self) -> u64;
    fn usage_projection_cost_floor_enabled(&self) -> bool;
    fn usage_projection_cost_floor_margin_percent(&self) -> u32;
}

impl AccountRuntimeConfigExt for AccountRuntimeConfig {
    fn account_runtime_enabled(&self) -> bool {
        self.external_pools_enabled
    }

    fn account_route_allowed(&self, endpoint: &str) -> bool {
        self.external_pool_route_allowed(endpoint)
    }

    fn effective_account_dispatch_max_wait_secs(&self) -> u64 {
        self.effective_dispatch_max_wait_secs()
    }

    fn account_direct_policy_enabled(&self) -> bool {
        self.external_direct_policy_enabled
    }

    fn account_local_rescue_enabled(&self) -> bool {
        self.external_pool_local_rescue_enabled
    }

    fn account_local_rescue_on_rate_limit(&self) -> bool {
        self.external_pool_local_rescue_on_rate_limit
    }

    fn account_local_rescue_on_timeout(&self) -> bool {
        self.external_pool_local_rescue_on_timeout
    }

    fn account_local_rescue_on_capacity(&self) -> bool {
        self.external_pool_local_rescue_on_capacity
    }

    fn account_local_rescue_max_wait_secs(&self) -> u64 {
        self.external_pool_local_rescue_max_wait_secs
    }

    fn clamped_account_request_timeout_secs(&self) -> u64 {
        self.external_pool_request_timeout_secs.clamp(1, 60)
    }

    fn usage_projection_cost_floor_enabled(&self) -> bool {
        self.external_pool_usage_projection_cost_floor_enabled
    }

    fn usage_projection_cost_floor_margin_percent(&self) -> u32 {
        self.external_pool_usage_projection_cost_floor_margin_percent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_runtime_config_ext_applies_enablement_and_route_policy() {
        let mut config = AccountRuntimeConfig::default();
        assert!(!config.account_runtime_enabled_for_endpoint("/cc/v1/messages"));

        config.external_pools_enabled = true;
        assert!(config.account_runtime_enabled_for_endpoint("/cc/v1/messages"));

        config.external_pool_route_mode = AccountRouteMode::AllowList;
        config.external_pool_route_rules = vec!["/dfcache/team-a".to_string()];

        assert!(!config.account_runtime_enabled_for_endpoint("/cc/v1/messages"));
        assert!(config.account_runtime_enabled_for_endpoint("/dfcache/team-a/v1/messages"));
    }
}
