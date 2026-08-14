//! Account runtime facade.
//!
//! The old implementation still lives behind the external-pool module while it
//! is being migrated. New integration points should depend on this account
//! runtime boundary instead of importing the legacy scheduler manager directly.

pub type AccountRuntimeManager = crate::external_pool::ExternalPoolManager;

pub type AccountRuntimeConfig = crate::model::config::ExternalPoolsConfig;

pub type UpstreamAccountStorageRecord = crate::external_pool::ExternalPool;

pub type UpstreamAccountStatusRecord = crate::external_pool::ExternalPoolStatus;

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
    fn legacy_local_rescue_max_wait_secs(&self) -> u64;
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

    fn legacy_local_rescue_max_wait_secs(&self) -> u64 {
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
    use crate::model::config::ExternalPoolRouteMode;

    #[test]
    fn account_runtime_config_ext_applies_enablement_and_route_policy() {
        let mut config = AccountRuntimeConfig::default();
        assert!(!config.account_runtime_enabled_for_endpoint("/cc/v1/messages"));

        config.external_pools_enabled = true;
        assert!(config.account_runtime_enabled_for_endpoint("/cc/v1/messages"));

        config.external_pool_route_mode = ExternalPoolRouteMode::AllowList;
        config.external_pool_route_rules = vec!["/dfcache/team-a".to_string()];

        assert!(!config.account_runtime_enabled_for_endpoint("/cc/v1/messages"));
        assert!(config.account_runtime_enabled_for_endpoint("/dfcache/team-a/v1/messages"));
    }
}
