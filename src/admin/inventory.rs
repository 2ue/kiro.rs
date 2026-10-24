//! Account inventory: accounts kept outside the scheduler.
//!
//! The inventory has its own table and its own renewal worker. It reuses the credential parsing
//! and normalisation of the pool import, and the token refresh and quota query helpers, but it
//! never touches the scheduler state. Pushing an account calls the regular pool import; after a
//! successful push the inventory row keeps no secrets, only the id of the pool account, so a
//! refresh token is never held (and refreshed) in two places.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::error::AdminServiceError;
use super::service::{AdminService, balance_response_from_usage_public};
use super::types::{AddCredentialRequest, BalanceResponse, BatchCredentialImportDefaults};
use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::token_manager::{MultiTokenManager, RefreshFailure, RefreshFailureKind};
use crate::storage::credential_inventory::{
    InventoryListFilter, InventoryRow, InventoryStore, NewInventoryRow, RenewOutcome,
    STATUS_INVALID, STATUS_PUSHED, STATUS_PUSHING, STATUS_STORED,
};

const MAX_IMPORT: usize = 500;
const MAX_PAGE_SIZE: usize = 100;
/// A `pushing` row older than this was left by a crash and is reconciled at startup.
const STALE_PUSH_SECS: i64 = 300;
const RENEW_ROWS_PER_PASS: usize = 200;

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}

/// Secret hashes as the pool computes them, so duplicates are detected across both tables.
fn secret_hashes(credential: &KiroCredentials) -> (String, Option<String>, Option<String>) {
    let is_api_key = credential.is_api_key_credential();
    let auth_kind = if is_api_key {
        "api_key".to_string()
    } else {
        credential
            .auth_method
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("oauth")
            .to_ascii_lowercase()
    };
    let api_key_hash = is_api_key
        .then_some(credential.kiro_api_key.as_deref())
        .flatten()
        .filter(|value| !value.is_empty())
        .map(sha256_hex);
    let refresh_token_hash = (!is_api_key)
        .then_some(credential.refresh_token.as_deref())
        .flatten()
        .filter(|value| !value.is_empty())
        .map(sha256_hex);
    (auth_kind, api_key_hash, refresh_token_hash)
}

/// The same account after a refresh-token rotation still has the same login: auth method plus
/// e-mail or profile ARN. Hashes alone would miss it.
fn identity_key(credential: &KiroCredentials) -> Option<String> {
    let method = credential
        .auth_method
        .as_deref()
        .unwrap_or("oauth")
        .to_ascii_lowercase();
    if let Some(email) = credential
        .email
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty())
    {
        return Some(format!("{method}|email|{}", email.to_lowercase()));
    }
    credential
        .profile_arn
        .as_deref()
        .map(str::trim)
        .filter(|arn| !arn.is_empty())
        .map(|arn| format!("{method}|arn|{arn}"))
}

fn is_invalid_grant(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<RefreshFailure>()
        .is_some_and(|failure| failure.kind == RefreshFailureKind::InvalidGrant)
}

fn credential_from_row(row: &InventoryRow) -> Result<KiroCredentials, AdminServiceError> {
    let data = row.data.clone().ok_or_else(|| {
        AdminServiceError::Conflict(format!("库存账号 #{} 没有可用的凭据数据", row.id))
    })?;
    serde_json::from_value(data).map_err(|e| {
        AdminServiceError::InternalError(format!("库存账号 #{} 数据损坏: {e}", row.id))
    })
}

fn credential_json(credential: &KiroCredentials) -> Result<serde_json::Value, AdminServiceError> {
    let mut value = serde_json::to_value(credential)
        .map_err(|e| AdminServiceError::InternalError(format!("序列化凭据失败: {e}")))?;
    if let Some(object) = value.as_object_mut() {
        object.remove("id");
    }
    Ok(value)
}

// ---------------------------------------------------------------------------
// API types
// ---------------------------------------------------------------------------

/// Region and proxy for an inventory import. Scheduling parameters are chosen at push time.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryImportDefaults {
    #[serde(default)]
    pub auth_region: Option<String>,
    #[serde(default)]
    pub api_region: Option<String>,
    #[serde(default)]
    pub proxy_resource_id: Option<u64>,
    /// Several proxy resources are assigned round-robin in import order.
    #[serde(default)]
    pub proxy_resource_ids: Vec<u64>,
    #[serde(default)]
    pub proxy_url: Option<String>,
    #[serde(default)]
    pub proxy_username: Option<String>,
    #[serde(default)]
    pub proxy_password: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryImportRequest {
    pub credentials: Vec<AddCredentialRequest>,
    #[serde(default)]
    pub defaults: InventoryImportDefaults,
    /// Refresh once and query quota and subscription right after import. On by default.
    #[serde(default = "default_true")]
    pub check: bool,
    #[serde(default)]
    pub source: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryImportItem {
    pub index: usize,
    pub ok: bool,
    pub skipped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inventory_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryImportResponse {
    pub total: usize,
    pub imported: usize,
    pub skipped: usize,
    pub failed: usize,
    pub items: Vec<InventoryImportItem>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryListQuery {
    #[serde(default)]
    pub page: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub q: Option<String>,
}

/// State of the pool account an inventory row was pushed to.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryPoolLink {
    pub credential_id: u64,
    /// `false` once the pool account was deleted.
    pub exists: bool,
    pub disabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscription_title: Option<String>,
}

/// An inventory account as shown in the console. Secrets are never included.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryItem {
    pub id: u64,
    pub status: String,
    pub auth_method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    pub has_proxy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_resource_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_resource_name: Option<String>,
    /// Whether the bound proxy can still be used: the resource exists and is enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_usable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_info: Option<BalanceResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_check_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_check_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_refresh_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_refresh_error: Option<String>,
    pub refresh_failure_count: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool: Option<InventoryPoolLink>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pushed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryCountsResponse {
    pub total: u64,
    pub stored: u64,
    pub invalid: u64,
    pub pushing: u64,
    pub pushed: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryListResponse {
    pub page: usize,
    pub limit: usize,
    pub total: u64,
    pub total_pages: usize,
    pub counts: InventoryCountsResponse,
    pub items: Vec<InventoryItem>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryIdsRequest {
    pub ids: Vec<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryUpdateRequest {
    #[serde(default)]
    pub auth_region: Option<String>,
    #[serde(default)]
    pub api_region: Option<String>,
    /// Missing keeps the binding, `null` unbinds the proxy resource.
    #[serde(default, deserialize_with = "deserialize_present")]
    pub proxy_resource_id: Option<Option<u64>>,
    #[serde(default)]
    pub proxy_url: Option<String>,
    #[serde(default)]
    pub proxy_username: Option<String>,
    #[serde(default)]
    pub proxy_password: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

/// Push to the pool: the same parameters as the pool import, without the credential JSON.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryPushRequest {
    pub ids: Vec<u64>,
    #[serde(default)]
    pub defaults: BatchCredentialImportDefaults,
    #[serde(default)]
    pub auto_discover_supported_models: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryActionItem {
    pub id: u64,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryActionResponse {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub items: Vec<InventoryActionItem>,
}

impl InventoryActionResponse {
    fn from_items(items: Vec<InventoryActionItem>) -> Self {
        let succeeded = items.iter().filter(|item| item.ok).count();
        Self {
            total: items.len(),
            succeeded,
            failed: items.len() - succeeded,
            items,
        }
    }
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

impl AdminService {
    fn inventory_store(&self) -> InventoryStore {
        InventoryStore::new(self.inventory_pool())
    }

    fn inventory_item(&self, row: InventoryRow, pool: &PoolLookup) -> InventoryItem {
        let credential = row
            .data
            .clone()
            .and_then(|data| serde_json::from_value::<KiroCredentials>(data).ok());
        let manager = self.inventory_token_manager();
        let proxy_resource_id = credential.as_ref().and_then(|c| c.proxy_resource_id);
        let proxy_url = credential
            .as_ref()
            .and_then(|c| c.proxy_url.clone())
            .filter(|url| !url.trim().is_empty())
            .map(|url| mask_proxy_url(&url));
        let account_info = row
            .account_info
            .clone()
            .and_then(|value| serde_json::from_value::<BalanceResponse>(value).ok());
        InventoryItem {
            id: row.id,
            status: row.status.clone(),
            auth_method: row.auth_kind.clone(),
            email: row.email.clone(),
            provider: credential.as_ref().and_then(|c| c.provider.clone()),
            auth_region: credential
                .as_ref()
                .and_then(|c| c.auth_region.clone().or(c.region.clone())),
            api_region: credential.as_ref().and_then(|c| c.api_region.clone()),
            machine_id: credential.as_ref().and_then(|c| c.machine_id.clone()),
            has_proxy: proxy_resource_id.is_some() || proxy_url.is_some(),
            proxy_resource_id,
            proxy_resource_name: proxy_resource_id
                .and_then(|id| pool.proxy_names.get(&id).cloned()),
            proxy_usable: proxy_resource_id.map(|id| manager.proxy_resource_enabled(id)),
            proxy_url,
            expires_at: credential.as_ref().and_then(|c| c.expires_at.clone()),
            account_info,
            last_check_at: row.last_check_at,
            last_check_error: row.last_check_error.clone(),
            last_refresh_at: row.last_refresh_at,
            last_refresh_error: row.last_refresh_error.clone(),
            refresh_failure_count: row.refresh_failure_count,
            source: row.source.clone(),
            note: row.note.clone(),
            pool: row.pushed_credential_id.map(|id| pool.link(id)),
            pushed_at: row.pushed_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }

    fn pool_lookup(&self) -> PoolLookup {
        let snapshot = self.inventory_token_manager().base_snapshot();
        let mut accounts = std::collections::HashMap::new();
        let mut hashes = HashSet::new();
        let mut identities = HashSet::new();
        let mut proxy_names = std::collections::HashMap::new();
        for entry in snapshot.entries {
            if let Some(hash) = entry.refresh_token_hash.clone() {
                hashes.insert(hash);
            }
            if let Some(hash) = entry.api_key_hash.clone() {
                hashes.insert(hash);
            }
            let method = entry
                .auth_method
                .as_deref()
                .unwrap_or("oauth")
                .to_ascii_lowercase();
            if let Some(email) = entry
                .email
                .as_deref()
                .map(str::trim)
                .filter(|e| !e.is_empty())
            {
                identities.insert(format!("{method}|email|{}", email.to_lowercase()));
            }
            if let (Some(id), Some(name)) =
                (entry.proxy_resource_id, entry.proxy_resource_name.clone())
            {
                proxy_names.insert(id, name);
            }
            accounts.insert(
                entry.id,
                (
                    entry.disabled,
                    entry.disabled_reason.clone(),
                    entry.subscription_title.clone(),
                ),
            );
        }
        if let Ok(resources) = super::service::block_on_admin_store_public(
            self.inventory_postgres().load_proxy_resources(),
        ) {
            for resource in resources {
                proxy_names.entry(resource.id).or_insert(resource.name);
            }
        }
        PoolLookup {
            accounts,
            hashes,
            identities,
            proxy_names,
        }
    }

    pub fn list_inventory(
        &self,
        query: InventoryListQuery,
    ) -> Result<InventoryListResponse, AdminServiceError> {
        let limit = query.limit.unwrap_or(20).clamp(1, MAX_PAGE_SIZE);
        let page = query.page.unwrap_or(1).max(1);
        let status = query
            .status
            .filter(|s| !s.trim().is_empty() && s != "__all__")
            .filter(|s| {
                [STATUS_STORED, STATUS_INVALID, STATUS_PUSHING, STATUS_PUSHED].contains(&s.as_str())
            });
        let store = self.inventory_store();
        let filter = InventoryListFilter {
            status,
            query: query.q,
            limit: limit as i64,
            offset: ((page - 1) * limit) as i64,
        };
        let (rows, total) = super::service::block_on_admin_store_public(store.list(&filter))
            .map_err(|e| AdminServiceError::InternalError(format!("读取库存失败: {e}")))?;
        let counts = super::service::block_on_admin_store_public(store.counts())
            .map_err(|e| AdminServiceError::InternalError(format!("读取库存失败: {e}")))?;
        let pool = self.pool_lookup();
        Ok(InventoryListResponse {
            page,
            limit,
            total,
            total_pages: (total as usize).div_ceil(limit).max(1),
            counts: InventoryCountsResponse {
                total: counts.total,
                stored: counts.stored,
                invalid: counts.invalid,
                pushing: counts.pushing,
                pushed: counts.pushed,
            },
            items: rows
                .into_iter()
                .map(|row| self.inventory_item(row, &pool))
                .collect(),
        })
    }

    pub async fn import_inventory(
        &self,
        req: InventoryImportRequest,
    ) -> Result<InventoryImportResponse, AdminServiceError> {
        if req.credentials.is_empty() {
            return Err(AdminServiceError::InvalidCredential(
                "没有可导入的账号".to_string(),
            ));
        }
        if req.credentials.len() > MAX_IMPORT {
            return Err(AdminServiceError::InvalidCredential(format!(
                "单次最多导入 {MAX_IMPORT} 个账号"
            )));
        }
        let rotation: Vec<u64> = {
            let mut seen = HashSet::new();
            req.defaults
                .proxy_resource_ids
                .iter()
                .copied()
                .filter(|id| seen.insert(*id))
                .collect()
        };
        let mut wanted_proxies = rotation.clone();
        wanted_proxies.extend(req.defaults.proxy_resource_id);
        let missing = self
            .inventory_token_manager()
            .missing_proxy_resource_ids(&wanted_proxies);
        if !missing.is_empty() {
            return Err(AdminServiceError::InvalidCredential(format!(
                "代理资源不存在或已删除: {missing:?}"
            )));
        }

        let store = self.inventory_store();
        let pool = self.pool_lookup();
        let total = req.credentials.len();
        let mut items = Vec::with_capacity(total);
        let mut seen_in_batch = HashSet::new();
        for (index, mut request) in req.credentials.into_iter().enumerate() {
            let import_index = index + 1;
            apply_inventory_defaults(
                &mut request,
                &req.defaults,
                rotation.get(index % rotation.len().max(1)).copied(),
            );
            // Scheduling parameters belong to the pool; they are chosen when the account is pushed.
            request.priority = 0;
            request.fast = 0;
            request.max_concurrent_requests = None;
            request.rpm = None;
            request.supported_models = Vec::new();
            let email = request.email.clone();
            let mut credential = match self.inventory_credential_from_request(request) {
                Ok(credential) => credential,
                Err(err) => {
                    items.push(InventoryImportItem {
                        index: import_index,
                        ok: false,
                        skipped: false,
                        inventory_id: None,
                        email,
                        message: Some(err.to_string()),
                    });
                    continue;
                }
            };
            let (auth_kind, api_key_hash, refresh_token_hash) = secret_hashes(&credential);
            let identity = identity_key(&credential);
            let secret = refresh_token_hash.clone().or(api_key_hash.clone());
            if let Some(reason) = duplicate_reason(
                &pool,
                secret.as_deref(),
                identity.as_deref(),
                &mut seen_in_batch,
            ) {
                items.push(InventoryImportItem {
                    index: import_index,
                    ok: false,
                    skipped: true,
                    inventory_id: None,
                    email,
                    message: Some(reason),
                });
                continue;
            }
            match super::service::block_on_admin_store_public(store.find_duplicates(
                refresh_token_hash.as_deref(),
                api_key_hash.as_deref(),
                identity.as_deref(),
            )) {
                Ok(found) if !found.is_empty() => {
                    let (id, status) = &found[0];
                    let reason = if status == STATUS_PUSHED {
                        format!("已在库存 #{id} 记录，并已推送到账号池")
                    } else {
                        format!("库存中已有该账号 #{id}")
                    };
                    items.push(InventoryImportItem {
                        index: import_index,
                        ok: false,
                        skipped: true,
                        inventory_id: Some(*id),
                        email,
                        message: Some(reason),
                    });
                    continue;
                }
                Ok(_) => {}
                Err(err) => {
                    items.push(InventoryImportItem {
                        index: import_index,
                        ok: false,
                        skipped: false,
                        inventory_id: None,
                        email,
                        message: Some(format!("查重失败: {err}")),
                    });
                    continue;
                }
            }
            // Fix the device fingerprint now, so the account keeps the same one once pushed.
            crate::kiro::machine_id::ensure_identity_fields(
                &mut credential,
                &self.inventory_runtime_config(),
            );
            let data = credential_json(&credential)?;
            let inserted =
                super::service::block_on_admin_store_public(store.insert(NewInventoryRow {
                    auth_kind: &auth_kind,
                    refresh_token_hash: refresh_token_hash.as_deref(),
                    api_key_hash: api_key_hash.as_deref(),
                    identity_key: identity.as_deref(),
                    email: credential.email.as_deref(),
                    data: &data,
                    source: req.source.as_deref(),
                    note: None,
                }));
            let row = match inserted {
                Ok(row) => row,
                Err(err) => {
                    let message = if err.to_string().contains("duplicate key") {
                        "库存中已有该账号".to_string()
                    } else {
                        format!("写入库存失败: {err}")
                    };
                    items.push(InventoryImportItem {
                        index: import_index,
                        ok: false,
                        skipped: true,
                        inventory_id: None,
                        email,
                        message: Some(message),
                    });
                    continue;
                }
            };
            let mut message = None;
            if req.check
                && let Err(err) = self.check_inventory_row(&store, row.clone()).await
            {
                message = Some(format!("已入库，查询失败: {err}"));
            }
            items.push(InventoryImportItem {
                index: import_index,
                ok: true,
                skipped: false,
                inventory_id: Some(row.id),
                email: credential.email.clone().or(email),
                message,
            });
        }
        self.audit(
            "inventory_import",
            "credential_inventory",
            None,
            true,
            None,
            json!({ "total": total, "imported": items.iter().filter(|i| i.ok).count() }),
        );
        let imported = items.iter().filter(|item| item.ok).count();
        let skipped = items.iter().filter(|item| item.skipped).count();
        Ok(InventoryImportResponse {
            total,
            imported,
            skipped,
            failed: total - imported - skipped,
            items,
        })
    }

    /// Refreshes the token (storing a rotated refresh token) and queries quota and subscription.
    async fn check_inventory_row(
        &self,
        store: &InventoryStore,
        row: InventoryRow,
    ) -> Result<(), AdminServiceError> {
        let credential = credential_from_row(&row)?;
        let manager = self.inventory_token_manager();
        let refreshed = if credential.is_api_key_credential() {
            credential
        } else {
            match manager.refresh_detached_credential(credential).await {
                Ok(refreshed) => {
                    let data = credential_json(&refreshed)?;
                    let (_, _, hash) = secret_hashes(&refreshed);
                    super::service::block_on_admin_store_public(store.save_refreshed(
                        row.id,
                        &data,
                        hash.as_deref(),
                    ))
                    .map_err(|e| {
                        AdminServiceError::InternalError(format!("保存续期结果失败: {e}"))
                    })?;
                    refreshed
                }
                Err(err) => {
                    let invalid = is_invalid_grant(&err);
                    let message = refresh_error_message(&err, invalid);
                    let _ = super::service::block_on_admin_store_public(
                        store.record_refresh_failure(row.id, &message, invalid),
                    );
                    let _ = super::service::block_on_admin_store_public(store.save_check(
                        row.id,
                        None,
                        Some(&message),
                        None,
                    ));
                    return Err(AdminServiceError::UpstreamError(message));
                }
            }
        };
        match manager
            .usage_limits_for_detached_credential(&refreshed)
            .await
        {
            Ok(usage) => {
                let email = usage.email().map(str::to_string);
                let balance = balance_response_from_usage_public(row.id, usage);
                let value = serde_json::to_value(&balance)
                    .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;
                super::service::block_on_admin_store_public(store.save_check(
                    row.id,
                    Some(&value),
                    None,
                    email.as_deref(),
                ))
                .map_err(|e| AdminServiceError::InternalError(format!("保存查询结果失败: {e}")))?;
                Ok(())
            }
            Err(err) => {
                let message = format!("查询额度失败: {err}");
                let _ = super::service::block_on_admin_store_public(store.save_check(
                    row.id,
                    None,
                    Some(&message),
                    None,
                ));
                Err(AdminServiceError::UpstreamError(message))
            }
        }
    }

    pub async fn check_inventory(
        &self,
        req: InventoryIdsRequest,
    ) -> Result<InventoryActionResponse, AdminServiceError> {
        let store = self.inventory_store();
        let mut items = Vec::with_capacity(req.ids.len());
        for id in dedup(req.ids) {
            let row = match super::service::block_on_admin_store_public(store.get(id)) {
                Ok(Some(row)) if row.status == STATUS_STORED || row.status == STATUS_INVALID => row,
                Ok(Some(row)) => {
                    items.push(InventoryActionItem {
                        id,
                        ok: false,
                        credential_id: row.pushed_credential_id,
                        message: Some("已推送到账号池的账号请在账号页查询".to_string()),
                    });
                    continue;
                }
                Ok(None) => {
                    items.push(InventoryActionItem {
                        id,
                        ok: false,
                        credential_id: None,
                        message: Some("库存账号不存在".to_string()),
                    });
                    continue;
                }
                Err(err) => {
                    items.push(InventoryActionItem {
                        id,
                        ok: false,
                        credential_id: None,
                        message: Some(err.to_string()),
                    });
                    continue;
                }
            };
            let result = self.check_inventory_row(&store, row).await;
            items.push(InventoryActionItem {
                id,
                ok: result.is_ok(),
                credential_id: None,
                message: result.err().map(|e| e.to_string()),
            });
        }
        Ok(InventoryActionResponse::from_items(items))
    }

    pub fn update_inventory(
        &self,
        id: u64,
        req: InventoryUpdateRequest,
    ) -> Result<InventoryItem, AdminServiceError> {
        let store = self.inventory_store();
        let row = super::service::block_on_admin_store_public(store.get(id))
            .map_err(|e| AdminServiceError::InternalError(e.to_string()))?
            .ok_or(AdminServiceError::NotFound { id })?;
        let mut credential = credential_from_row(&row)?;
        if let Some(region) = req.auth_region {
            credential.auth_region = Some(region.trim().to_string()).filter(|r| !r.is_empty());
        }
        if let Some(region) = req.api_region {
            credential.api_region = Some(region.trim().to_string()).filter(|r| !r.is_empty());
        }
        if let Some(resource) = req.proxy_resource_id {
            if let Some(resource_id) = resource {
                let missing = self
                    .inventory_token_manager()
                    .missing_proxy_resource_ids(&[resource_id]);
                if !missing.is_empty() {
                    return Err(AdminServiceError::InvalidCredential(format!(
                        "代理资源 #{resource_id} 不存在"
                    )));
                }
                credential.proxy_url = None;
                credential.proxy_username = None;
                credential.proxy_password = None;
            }
            credential.proxy_resource_id = resource;
        }
        if let Some(url) = req.proxy_url {
            let url = url.trim().to_string();
            credential.proxy_url = Some(url).filter(|u| !u.is_empty());
            if credential.proxy_url.is_some() {
                credential.proxy_resource_id = None;
            }
            credential.proxy_username = req.proxy_username.filter(|v| !v.is_empty());
            credential.proxy_password = req.proxy_password.filter(|v| !v.is_empty());
        }
        let data = credential_json(&credential)?;
        let note = req.note.as_deref().map(str::trim).filter(|n| !n.is_empty());
        let updated = super::service::block_on_admin_store_public(store.update_settings(
            id,
            &data,
            note.or(row.note.as_deref()),
        ))
        .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;
        if !updated {
            return Err(AdminServiceError::Conflict(
                "已推送或正在推送的账号不能修改".to_string(),
            ));
        }
        let row = super::service::block_on_admin_store_public(store.get(id))
            .map_err(|e| AdminServiceError::InternalError(e.to_string()))?
            .ok_or(AdminServiceError::NotFound { id })?;
        Ok(self.inventory_item(row, &self.pool_lookup()))
    }

    pub fn delete_inventory(
        &self,
        req: InventoryIdsRequest,
    ) -> Result<InventoryActionResponse, AdminServiceError> {
        let store = self.inventory_store();
        let items = dedup(req.ids)
            .into_iter()
            .map(
                |id| match super::service::block_on_admin_store_public(store.soft_delete(id)) {
                    Ok(true) => InventoryActionItem {
                        id,
                        ok: true,
                        credential_id: None,
                        message: None,
                    },
                    Ok(false) => InventoryActionItem {
                        id,
                        ok: false,
                        credential_id: None,
                        message: Some("不存在或正在推送".to_string()),
                    },
                    Err(err) => InventoryActionItem {
                        id,
                        ok: false,
                        credential_id: None,
                        message: Some(err.to_string()),
                    },
                },
            )
            .collect::<Vec<_>>();
        self.audit(
            "inventory_delete",
            "credential_inventory",
            None,
            true,
            None,
            json!({ "ids": items.iter().filter(|i| i.ok).map(|i| i.id).collect::<Vec<_>>() }),
        );
        Ok(InventoryActionResponse::from_items(items))
    }

    /// Moves accounts into the pool through the regular import. A bound proxy that can still be
    /// used is kept; the push defaults only fill in what the account does not have.
    pub async fn push_inventory(
        &self,
        req: InventoryPushRequest,
    ) -> Result<InventoryActionResponse, AdminServiceError> {
        if req.ids.is_empty() {
            return Err(AdminServiceError::InvalidCredential(
                "没有选择账号".to_string(),
            ));
        }
        if req.ids.len() > MAX_IMPORT {
            return Err(AdminServiceError::InvalidCredential(format!(
                "单次最多推送 {MAX_IMPORT} 个账号"
            )));
        }
        let store = self.inventory_store();
        let manager = self.inventory_token_manager();
        let rotation: Vec<u64> = {
            let mut seen = HashSet::new();
            req.defaults
                .proxy_resource_ids
                .iter()
                .copied()
                .filter(|id| seen.insert(*id))
                .collect()
        };
        let mut items = Vec::new();
        for (index, id) in dedup(req.ids).into_iter().enumerate() {
            let row = match super::service::block_on_admin_store_public(store.begin_push(id)) {
                Ok(Some(row)) => row,
                Ok(None) => {
                    items.push(InventoryActionItem {
                        id,
                        ok: false,
                        credential_id: None,
                        message: Some("账号不在库存中，或已推送、正在推送".to_string()),
                    });
                    continue;
                }
                Err(err) => {
                    items.push(InventoryActionItem {
                        id,
                        ok: false,
                        credential_id: None,
                        message: Some(err.to_string()),
                    });
                    continue;
                }
            };
            let outcome = async {
                let mut credential = credential_from_row(&row)?;
                // Keep a bound proxy resource only while it still exists and is enabled.
                if let Some(resource_id) = credential.proxy_resource_id
                    && !manager.proxy_resource_enabled(resource_id)
                {
                    credential.proxy_resource_id = None;
                }
                let mut request: AddCredentialRequest =
                    serde_json::from_value(credential_json(&credential)?).map_err(|e| {
                        AdminServiceError::InternalError(format!("转换库存账号失败: {e}"))
                    })?;
                request.disabled = None;
                let round_robin = rotation.get(index % rotation.len().max(1)).copied();
                let request = super::service::apply_batch_import_defaults_public(
                    request,
                    &req.defaults,
                    round_robin,
                );
                self.add_credential_for_inventory(request, req.auto_discover_supported_models)
                    .await
            }
            .await;
            match outcome {
                Ok(credential_id) => {
                    let _ = super::service::block_on_admin_store_public(
                        store.finish_push(id, credential_id),
                    );
                    items.push(InventoryActionItem {
                        id,
                        ok: true,
                        credential_id: Some(credential_id),
                        message: None,
                    });
                }
                Err(err) => {
                    let message = err.to_string();
                    let _ = super::service::block_on_admin_store_public(
                        store.abort_push(id, &format!("推送失败: {message}")),
                    );
                    items.push(InventoryActionItem {
                        id,
                        ok: false,
                        credential_id: None,
                        message: Some(message),
                    });
                }
            }
        }
        self.audit(
            "inventory_push",
            "credential_inventory",
            None,
            true,
            None,
            json!({ "pushed": items.iter().filter(|i| i.ok).map(|i| json!({ "inventoryId": i.id, "credentialId": i.credential_id })).collect::<Vec<_>>() }),
        );
        Ok(InventoryActionResponse::from_items(items))
    }

    /// Finishes pushes cut short by a crash: a row left in `pushing` is marked pushed when the pool
    /// already holds the account, and returned to the inventory otherwise.
    pub async fn reconcile_inventory_pushes(&self) -> anyhow::Result<usize> {
        let store = self.inventory_store();
        let rows = store.stale_pushing(STALE_PUSH_SECS).await?;
        let snapshot = self.inventory_token_manager().base_snapshot();
        let mut reconciled = 0;
        for row in rows {
            let pool_id = snapshot
                .entries
                .iter()
                .find(|entry| {
                    row.refresh_token_hash.is_some()
                        && entry.refresh_token_hash == row.refresh_token_hash
                        || row.api_key_hash.is_some() && entry.api_key_hash == row.api_key_hash
                })
                .map(|entry| entry.id);
            match pool_id {
                Some(credential_id) => store.finish_push(row.id, credential_id).await?,
                None => store.abort_push(row.id, "上次推送中断，已退回库存").await?,
            }
            reconciled += 1;
        }
        Ok(reconciled)
    }
}

/// Renews inventory tokens with the pool's rules: the same enable switch, scan interval and
/// refresh lead from the runtime config, and the same refresh path and proxy handling.
pub fn spawn_inventory_renewal_worker(
    manager: Arc<MultiTokenManager>,
    store: InventoryStore,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let first = manager
            .runtime_config()
            .token_refresh_background_interval_secs
            .max(1);
        tokio::time::sleep(StdDuration::from_secs(first)).await;
        loop {
            let config = manager.runtime_config();
            if config.token_refresh_background_enabled {
                let due_before = Utc::now()
                    + chrono::Duration::seconds(config.token_refresh_background_lead_secs as i64);
                let manager_for_pass = manager.clone();
                let result = store
                    .renew_due(due_before, RENEW_ROWS_PER_PASS, |row| {
                        let manager = manager_for_pass.clone();
                        async move { renew_row(&manager, row).await }
                    })
                    .await;
                match result {
                    Ok((renewed, failed)) if renewed > 0 || failed > 0 => {
                        tracing::info!(renewed, failed, "库存账号 Token 续期扫描完成");
                    }
                    Ok(_) => {}
                    Err(err) => {
                        tracing::warn!(error = %err, "库存账号 Token 续期扫描失败，下一轮重试")
                    }
                }
            }
            let interval = manager
                .runtime_config()
                .token_refresh_background_interval_secs
                .max(1);
            tokio::time::sleep(StdDuration::from_secs(interval)).await;
        }
    })
}

async fn renew_row(manager: &MultiTokenManager, row: InventoryRow) -> RenewOutcome {
    let credential = match row
        .data
        .clone()
        .map(serde_json::from_value::<KiroCredentials>)
    {
        Some(Ok(credential)) => credential,
        _ => {
            return RenewOutcome::Failed {
                error: "库存账号数据损坏".to_string(),
                invalid: true,
            };
        }
    };
    match manager.refresh_detached_credential(credential).await {
        Ok(refreshed) => match credential_json(&refreshed) {
            Ok(data) => {
                let (_, _, hash) = secret_hashes(&refreshed);
                RenewOutcome::Refreshed {
                    data,
                    refresh_token_hash: hash,
                }
            }
            Err(err) => RenewOutcome::Failed {
                error: err.to_string(),
                invalid: false,
            },
        },
        Err(err) => {
            let invalid = is_invalid_grant(&err);
            RenewOutcome::Failed {
                error: refresh_error_message(&err, invalid),
                invalid,
            }
        }
    }
}

fn refresh_error_message(err: &anyhow::Error, invalid: bool) -> String {
    if invalid {
        "refreshToken 已失效（invalid_grant），已停止续期".to_string()
    } else {
        format!("续期失败: {err}")
    }
}

struct PoolLookup {
    accounts: std::collections::HashMap<u64, (bool, Option<String>, Option<String>)>,
    hashes: HashSet<String>,
    identities: HashSet<String>,
    proxy_names: std::collections::HashMap<u64, String>,
}

impl PoolLookup {
    fn link(&self, credential_id: u64) -> InventoryPoolLink {
        match self.accounts.get(&credential_id) {
            Some((disabled, reason, subscription)) => InventoryPoolLink {
                credential_id,
                exists: true,
                disabled: *disabled,
                disabled_reason: reason.clone(),
                subscription_title: subscription.clone(),
            },
            None => InventoryPoolLink {
                credential_id,
                exists: false,
                disabled: true,
                disabled_reason: None,
                subscription_title: None,
            },
        }
    }
}

fn duplicate_reason(
    pool: &PoolLookup,
    secret_hash: Option<&str>,
    identity: Option<&str>,
    seen_in_batch: &mut HashSet<String>,
) -> Option<String> {
    if secret_hash.is_some_and(|hash| pool.hashes.contains(hash))
        || identity.is_some_and(|identity| pool.identities.contains(identity))
    {
        return Some("该账号已在账号池使用，不能再放入库存".to_string());
    }
    for key in [secret_hash, identity].into_iter().flatten() {
        if !seen_in_batch.insert(key.to_string()) {
            return Some("与本次导入中前面的账号重复".to_string());
        }
    }
    None
}

fn apply_inventory_defaults(
    request: &mut AddCredentialRequest,
    defaults: &InventoryImportDefaults,
    round_robin: Option<u64>,
) {
    let blank = |value: &Option<String>| value.as_deref().is_none_or(|v| v.trim().is_empty());
    if blank(&request.auth_region) && blank(&request.region) {
        request.auth_region = defaults
            .auth_region
            .clone()
            .filter(|v| !v.trim().is_empty());
    }
    if blank(&request.api_region) {
        request.api_region = defaults.api_region.clone().filter(|v| !v.trim().is_empty());
    }
    let has_own_proxy = request.proxy_resource_id.is_some() || !blank(&request.proxy_url);
    if !has_own_proxy {
        if let Some(resource_id) = round_robin.or(defaults.proxy_resource_id) {
            request.proxy_resource_id = Some(resource_id);
        } else if !blank(&defaults.proxy_url) {
            request.proxy_url = defaults.proxy_url.clone();
            request.proxy_username = defaults.proxy_username.clone();
            request.proxy_password = defaults.proxy_password.clone();
        }
    }
}

fn mask_proxy_url(url: &str) -> String {
    match url
        .split_once("://")
        .and_then(|(scheme, rest)| rest.rsplit_once('@').map(|(_, host)| (scheme, host)))
    {
        Some((scheme, host)) => format!("{scheme}://***@{host}"),
        None => url.to_string(),
    }
}

/// Tells a field set to `null` apart from a missing one (`#[serde(default)]` covers missing).
fn deserialize_present<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

fn dedup(ids: Vec<u64>) -> Vec<u64> {
    let mut seen = HashSet::new();
    ids.into_iter().filter(|id| seen.insert(*id)).collect()
}

pub(super) mod http {
    use axum::{
        Json,
        extract::{Path, Query, State},
        response::{IntoResponse, Response},
    };

    use super::{
        InventoryIdsRequest, InventoryImportRequest, InventoryListQuery, InventoryPushRequest,
        InventoryUpdateRequest,
    };
    use crate::admin::error::AdminServiceError;
    use crate::admin::middleware::AdminState;

    fn respond<T: serde::Serialize>(result: Result<T, AdminServiceError>) -> Response {
        match result {
            Ok(value) => Json(value).into_response(),
            Err(err) => (err.status_code(), Json(err.into_response())).into_response(),
        }
    }

    /// GET /api/admin/inventory
    pub async fn list(
        State(state): State<AdminState>,
        Query(query): Query<InventoryListQuery>,
    ) -> Response {
        respond(state.service.list_inventory(query))
    }

    /// POST /api/admin/inventory/import
    pub async fn import(
        State(state): State<AdminState>,
        Json(payload): Json<InventoryImportRequest>,
    ) -> Response {
        respond(state.service.import_inventory(payload).await)
    }

    /// POST /api/admin/inventory/check
    pub async fn check(
        State(state): State<AdminState>,
        Json(payload): Json<InventoryIdsRequest>,
    ) -> Response {
        respond(state.service.check_inventory(payload).await)
    }

    /// POST /api/admin/inventory/push
    pub async fn push(
        State(state): State<AdminState>,
        Json(payload): Json<InventoryPushRequest>,
    ) -> Response {
        respond(state.service.push_inventory(payload).await)
    }

    /// POST /api/admin/inventory/delete
    pub async fn delete(
        State(state): State<AdminState>,
        Json(payload): Json<InventoryIdsRequest>,
    ) -> Response {
        respond(state.service.delete_inventory(payload))
    }

    /// PUT /api/admin/inventory/{id}
    pub async fn update(
        State(state): State<AdminState>,
        Path(id): Path<u64>,
        Json(payload): Json<InventoryUpdateRequest>,
    ) -> Response {
        respond(state.service.update_inventory(id, payload))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credential(email: Option<&str>, refresh: Option<&str>) -> KiroCredentials {
        KiroCredentials {
            auth_method: Some("social".to_string()),
            email: email.map(str::to_string),
            refresh_token: refresh.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn identity_key_survives_refresh_token_rotation() {
        let before = credential(Some("User@Example.com"), Some("rt-1"));
        let after = credential(Some("user@example.com "), Some("rt-2"));
        assert_eq!(identity_key(&before), identity_key(&after));
        assert_ne!(secret_hashes(&before).2, secret_hashes(&after).2);
    }

    #[test]
    fn duplicates_in_pool_and_in_batch_are_rejected() {
        let mut pool = PoolLookup {
            accounts: Default::default(),
            hashes: HashSet::from(["h-pool".to_string()]),
            identities: HashSet::from(["social|email|a@example.com".to_string()]),
            proxy_names: Default::default(),
        };
        let mut seen = HashSet::new();
        assert!(
            duplicate_reason(&pool, Some("h-pool"), None, &mut seen)
                .unwrap()
                .contains("账号池")
        );
        assert!(
            duplicate_reason(
                &pool,
                Some("h-x"),
                Some("social|email|a@example.com"),
                &mut seen
            )
            .unwrap()
            .contains("账号池")
        );
        assert!(
            duplicate_reason(
                &pool,
                Some("h-new"),
                Some("social|email|b@example.com"),
                &mut seen
            )
            .is_none()
        );
        assert!(
            duplicate_reason(
                &pool,
                Some("h-other"),
                Some("social|email|b@example.com"),
                &mut seen
            )
            .unwrap()
            .contains("本次导入")
        );
        pool.hashes.clear();
    }

    #[test]
    fn inventory_defaults_keep_the_accounts_own_proxy_and_region() {
        let defaults = InventoryImportDefaults {
            auth_region: Some("eu-central-1".to_string()),
            api_region: Some("us-east-1".to_string()),
            proxy_resource_id: Some(7),
            ..Default::default()
        };
        let mut own: AddCredentialRequest = serde_json::from_value(json!({
            "refreshToken": "rt", "authRegion": "us-west-2", "proxyUrl": "socks5h://u:p@host:1"
        }))
        .unwrap();
        apply_inventory_defaults(&mut own, &defaults, None);
        assert_eq!(own.auth_region.as_deref(), Some("us-west-2"));
        assert_eq!(own.api_region.as_deref(), Some("us-east-1"));
        assert_eq!(own.proxy_resource_id, None);

        let mut bare: AddCredentialRequest =
            serde_json::from_value(json!({ "refreshToken": "rt" })).unwrap();
        apply_inventory_defaults(&mut bare, &defaults, Some(9));
        assert_eq!(bare.auth_region.as_deref(), Some("eu-central-1"));
        assert_eq!(bare.proxy_resource_id, Some(9));
    }

    #[test]
    fn proxy_unbind_is_distinct_from_missing() {
        let unbind: InventoryUpdateRequest =
            serde_json::from_value(json!({ "proxyResourceId": null })).unwrap();
        assert_eq!(unbind.proxy_resource_id, Some(None));
        let missing: InventoryUpdateRequest =
            serde_json::from_value(json!({ "note": "x" })).unwrap();
        assert_eq!(missing.proxy_resource_id, None);
        let bind: InventoryUpdateRequest =
            serde_json::from_value(json!({ "proxyResourceId": 4 })).unwrap();
        assert_eq!(bind.proxy_resource_id, Some(Some(4)));
    }

    #[test]
    fn proxy_credentials_are_masked() {
        assert_eq!(
            mask_proxy_url("socks5h://user:pass@1.2.3.4:1080"),
            "socks5h://***@1.2.3.4:1080"
        );
        assert_eq!(mask_proxy_url("http://1.2.3.4:8080"), "http://1.2.3.4:8080");
    }
}
