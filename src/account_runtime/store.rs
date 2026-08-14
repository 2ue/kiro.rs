//! Account-named storage bridge for the migration period.
//!
//! The current persisted table and storage implementation still use legacy
//! names. New account-runtime callers should use this bridge so those names
//! stay confined to the compatibility layer until the schema moves.

use crate::{
    account_runtime::UpstreamAccountStorageRecord,
    external_pool::{CreateExternalPoolRequest, UpdateExternalPoolRequest},
    storage::postgres::PostgresStore,
};

pub type CreateUpstreamAccountStorageRequest = CreateExternalPoolRequest;

pub type UpdateUpstreamAccountStorageRequest = UpdateExternalPoolRequest;

impl PostgresStore {
    pub async fn list_upstream_account_records(
        &self,
        mask_secrets: bool,
    ) -> anyhow::Result<Vec<UpstreamAccountStorageRecord>> {
        self.list_external_pools(mask_secrets).await
    }

    pub async fn get_upstream_account_record(
        &self,
        id: u64,
        mask_secrets: bool,
    ) -> anyhow::Result<Option<UpstreamAccountStorageRecord>> {
        self.get_external_pool(id, mask_secrets).await
    }

    pub async fn create_upstream_account_record_unmasked(
        &self,
        request: CreateUpstreamAccountStorageRequest,
    ) -> anyhow::Result<UpstreamAccountStorageRecord> {
        self.create_external_pool_unmasked(request).await
    }

    pub async fn update_upstream_account_record_unmasked(
        &self,
        id: u64,
        request: UpdateUpstreamAccountStorageRequest,
    ) -> anyhow::Result<Option<UpstreamAccountStorageRecord>> {
        self.update_external_pool_unmasked(id, request).await
    }

    pub async fn set_upstream_account_enabled_unmasked(
        &self,
        id: u64,
        enabled: bool,
    ) -> anyhow::Result<Option<UpstreamAccountStorageRecord>> {
        self.set_external_pool_enabled_unmasked(id, enabled).await
    }

    pub async fn set_upstream_account_supported_models_unmasked(
        &self,
        id: u64,
        supported_models: Vec<String>,
    ) -> anyhow::Result<Option<UpstreamAccountStorageRecord>> {
        self.set_external_pool_supported_models_unmasked(id, supported_models)
            .await
    }

    pub async fn delete_upstream_account_record(&self, id: u64) -> anyhow::Result<bool> {
        self.soft_delete_external_pool(id).await
    }

    pub async fn clear_upstream_account_auto_disabled_unmasked(
        &self,
        id: u64,
    ) -> anyhow::Result<Option<UpstreamAccountStorageRecord>> {
        self.clear_external_pool_auto_disabled_unmasked(id).await
    }
}
