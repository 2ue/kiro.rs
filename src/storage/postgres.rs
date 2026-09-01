use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Timelike, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{
    Connection, PgPool, Postgres, QueryBuilder, Row, Transaction,
    postgres::{PgConnection, PgPoolOptions, PgRow},
};
use uuid::Uuid;

#[cfg(test)]
use crate::anthropic::model_capabilities::KiroReasoningFieldPath;
use crate::anthropic::model_capabilities::{
    KiroReasoningFieldCapability, MANUAL_SOURCE, ModelCapabilitiesStatus, ModelCapabilityItem,
};
use crate::anthropic::pricing::{
    MANUAL_PRICING_SOURCE, ModelPriceItem, ModelPricing, PricingStatus,
};
use crate::anthropic::usage::{
    CredentialCostSummary, REALTIME_USAGE_WINDOW_SECS, UsageAggregate, UsageBreakdownItem,
    UsageDashboardCredentialAggregate, UsageDashboardResponse, UsageDashboardSeries,
    UsageDashboardSummary, UsageDashboardTop, UsageDashboardWindow, UsageDashboardWindowSpec,
    UsageExternalPoolBillingByPool, UsageExternalPoolBillingSummary, UsageExternalPoolRiskBucket,
    UsageExternalPoolRiskCacheStats, UsageExternalPoolRiskCostConfig,
    UsageExternalPoolRiskCostStats, UsageExternalPoolRiskFilters, UsageExternalPoolRiskGroup,
    UsageExternalPoolRiskQuery, UsageExternalPoolRiskResponse, UsageExternalPoolRiskSample,
    UsageExternalPoolRiskThresholds, UsageExternalPoolRiskTotals, UsageExternalPoolRiskWindow,
    UsageRealtimeStats, UsageRecord, UsageRecordQuery, UsageRecordStatus, UsageRecordsPageResult,
    UsageRecordsResult, UsageRouteKind, UsageSeriesPoint, UsageSource, UsageSummary,
    UsageTopAggregate, usage_dashboard_daily_windows, usage_dashboard_hourly_windows,
    usage_dashboard_timezone, usage_dashboard_window_spec_for_key, usage_dashboard_windows,
};
use crate::external_pool::{
    CreateExternalPoolRequest, ExternalPool, ExternalPoolAuthType, ExternalPoolAutoDisablePolicy,
    ExternalPoolEligibility, ExternalPoolHeaderProfile, ExternalPoolModelMappingMode,
    ExternalPoolRawModelMode, ExternalPoolRequestBodyMode, ExternalPoolStreamRetryMode,
    ExternalPoolTlsProfile, ExternalPoolUsageProjectionMode, ExternalPoolWireProfile,
    UpdateExternalPoolRequest, mask_external_pool_key, normalize_external_pool_header_overrides,
    normalize_external_pool_model_mapping_rules,
};
use crate::kiro::model::available_models::KiroModelCapabilityCohortKey;
use crate::kiro::model::credentials::KiroCredentials;
use crate::model::config::{
    Config, ExternalPoolRouteMode, ExternalPoolStreamResponseMode, ModelMappingRule,
    normalize_route_rules,
};
use crate::model::model_support::normalize_supported_models;

const ACTIVE_CREDENTIALS_SELECT_SQL: &str = r#"
SELECT id, priority, disabled, data, created_at, updated_at, revision
FROM credentials
WHERE deleted_at IS NULL
ORDER BY priority ASC, id ASC
"#;

const CREDENTIAL_RUNTIME_STATE_SELECT_SQL: &str = r#"
SELECT credential_id, failure_count, refresh_failure_count,
       disabled_reason, warmup_remaining, generation, revision
FROM credential_runtime_state
"#;

const EXTERNAL_POOL_SELECT_SQL: &str = r#"
SELECT id, name, base_url, api_key, auth_type, enabled, priority,
       max_concurrent_requests, usage_projection_mode, stream_response_mode,
       request_body_mode, raw_model_mode, auto_disable_policy, pre_output_stream_retry_mode,
       header_profile, append_beta_query, header_overrides,
       wire_profile, tls_profile,
       auto_disabled, auto_disabled_reason, auto_disabled_at,
       auto_disabled_until, auto_disabled_last_error, preserve_path,
       normalize_model_version_dots, model_mapping_mode,
       model_mapping_require_match, model_mapping_rules, supported_models,
       route_mode, route_rules, notes,
       revision, created_at, updated_at
FROM external_upstream_pools
WHERE deleted_at IS NULL
ORDER BY priority ASC, id ASC
"#;

const CREDENTIAL_ID_SEQUENCE_LOCK_ID: i64 = 4_950_531_234_002;
const POSTGRES_MIGRATION_LOCK_ID: i64 = 4_950_531_234_001;
const USAGE_CLEANUP_COMMIT_LOCK_ID: i64 = 4_950_531_234_003;
const USAGE_OFFLINE_MAINTENANCE_LOCK_ID: i64 = 4_950_531_234_004;
const USAGE_RECORD_COMMIT_LOCK_DOMAIN: i64 = 0x0055_5341_4745_4944;
const USAGE_INDEX_STARTUP_MAX_BYTES: i64 = 64 * 1024 * 1024;
const USAGE_SOFT_DELETE_WATERMARK_SCOPE: &str = "soft_delete_created_at";
const USAGE_CLEANUP_LOCK_TIMEOUT_MS: u64 = 250;
const USAGE_CLEANUP_STATEMENT_TIMEOUT_MS: u64 = 10_000;
const USAGE_DASHBOARD_STATEMENT_TIMEOUT_MS: u64 = 115_000;
const USAGE_DASHBOARD_LOCK_TIMEOUT_MS: u64 = 250;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RequiredPostgresColumn {
    table_name: &'static str,
    column_name: &'static str,
}

const REQUIRED_POSTGRES_SCHEMA_COLUMNS: &[RequiredPostgresColumn] = &[
    RequiredPostgresColumn {
        table_name: "credentials",
        column_name: "revision",
    },
    RequiredPostgresColumn {
        table_name: "credential_runtime_state",
        column_name: "revision",
    },
    RequiredPostgresColumn {
        table_name: "credential_runtime_state",
        column_name: "generation",
    },
    RequiredPostgresColumn {
        table_name: "credential_runtime_mutations",
        column_name: "operation_id",
    },
    RequiredPostgresColumn {
        table_name: "credential_stats_delta_batches",
        column_name: "operation_id",
    },
    RequiredPostgresColumn {
        table_name: "external_upstream_pools",
        column_name: "revision",
    },
    RequiredPostgresColumn {
        table_name: "external_upstream_pools",
        column_name: "pre_output_stream_retry_mode",
    },
    RequiredPostgresColumn {
        table_name: "external_upstream_pools",
        column_name: "route_mode",
    },
    RequiredPostgresColumn {
        table_name: "external_upstream_pools",
        column_name: "route_rules",
    },
    RequiredPostgresColumn {
        table_name: "external_upstream_pools",
        column_name: "header_profile",
    },
    RequiredPostgresColumn {
        table_name: "external_upstream_pools",
        column_name: "append_beta_query",
    },
    RequiredPostgresColumn {
        table_name: "external_upstream_pools",
        column_name: "header_overrides",
    },
    RequiredPostgresColumn {
        table_name: "external_upstream_pools",
        column_name: "wire_profile",
    },
    RequiredPostgresColumn {
        table_name: "external_upstream_pools",
        column_name: "tls_profile",
    },
    RequiredPostgresColumn {
        table_name: "usage_records",
        column_name: "deleted_at",
    },
    RequiredPostgresColumn {
        table_name: "usage_records",
        column_name: "rollup_active",
    },
    RequiredPostgresColumn {
        table_name: "usage_cleanup_jobs",
        column_name: "batch_size",
    },
    RequiredPostgresColumn {
        table_name: "usage_records",
        column_name: "original_cost_usd",
    },
    RequiredPostgresColumn {
        table_name: "usage_records",
        column_name: "kiro_metering_usage",
    },
    RequiredPostgresColumn {
        table_name: "usage_records",
        column_name: "pricing_available",
    },
    RequiredPostgresColumn {
        table_name: "usage_records",
        column_name: "duration_ms",
    },
    RequiredPostgresColumn {
        table_name: "usage_records",
        column_name: "data",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_totals",
        column_name: "total_original_cost_usd",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_totals",
        column_name: "total_kiro_metering_usage",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_totals",
        column_name: "upstream_metadata_requests",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_totals",
        column_name: "sticky_bound_requests",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_totals",
        column_name: "fallback_from_sticky_requests",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_totals",
        column_name: "external_pool_raw_cost_usd",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_totals",
        column_name: "external_pool_shaped_cost_usd",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_totals",
        column_name: "external_pool_uplifted_cost_usd",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_totals",
        column_name: "external_pool_reported_cost_usd",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_totals",
        column_name: "external_pool_billable_cost_usd",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_totals",
        column_name: "external_pool_cost_floor_delta_usd",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_totals",
        column_name: "duration_ms_sum",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_totals",
        column_name: "duration_ms_count",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_totals",
        column_name: "duration_ms_max",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_time_buckets",
        column_name: "total_original_cost_usd",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_time_buckets",
        column_name: "total_kiro_metering_usage",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_time_buckets",
        column_name: "upstream_metadata_requests",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_time_buckets",
        column_name: "sticky_bound_requests",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_time_buckets",
        column_name: "fallback_from_sticky_requests",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_time_buckets",
        column_name: "external_pool_raw_cost_usd",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_time_buckets",
        column_name: "external_pool_shaped_cost_usd",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_time_buckets",
        column_name: "external_pool_uplifted_cost_usd",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_time_buckets",
        column_name: "external_pool_reported_cost_usd",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_time_buckets",
        column_name: "external_pool_billable_cost_usd",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_time_buckets",
        column_name: "external_pool_cost_floor_delta_usd",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_time_buckets",
        column_name: "duration_ms_sum",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_time_buckets",
        column_name: "duration_ms_count",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_time_buckets",
        column_name: "duration_ms_max",
    },
    RequiredPostgresColumn {
        table_name: "usage_duration_rollup_time_buckets",
        column_name: "duration_ms",
    },
    RequiredPostgresColumn {
        table_name: "usage_credential_cost_summary",
        column_name: "original_cost_usd",
    },
    RequiredPostgresColumn {
        table_name: "usage_credential_cost_summary",
        column_name: "kiro_metering_usage",
    },
    RequiredPostgresColumn {
        table_name: "usage_credential_cost_summary",
        column_name: "priced_requests",
    },
    RequiredPostgresColumn {
        table_name: "usage_credential_cost_summary",
        column_name: "unpriced_requests",
    },
    RequiredPostgresColumn {
        table_name: "model_capabilities_sync_status",
        column_name: "reasoning_fields",
    },
    RequiredPostgresColumn {
        table_name: "model_capabilities_sync_status",
        column_name: "reasoning_cohort_keys",
    },
    RequiredPostgresColumn {
        table_name: "model_capabilities_sync_status",
        column_name: "reasoning_cohort_complete",
    },
    RequiredPostgresColumn {
        table_name: "model_capabilities_sync_status",
        column_name: "reasoning_contract_version",
    },
    RequiredPostgresColumn {
        table_name: "model_capabilities_sync_status",
        column_name: "reasoning_invalid_models",
    },
];

fn required_postgres_schema_missing_columns(present: &HashSet<(String, String)>) -> Vec<String> {
    REQUIRED_POSTGRES_SCHEMA_COLUMNS
        .iter()
        .filter(|required| {
            !present.contains(&(
                required.table_name.to_string(),
                required.column_name.to_string(),
            ))
        })
        .map(|required| format!("{}.{}", required.table_name, required.column_name))
        .collect()
}

struct UsageIndexDefinition {
    table: &'static str,
    name: &'static str,
    sql: &'static str,
}

const USAGE_INDEX_DEFINITIONS: &[UsageIndexDefinition] = &[
    UsageIndexDefinition {
        table: "usage_records",
        name: "idx_usage_records_created_at",
        sql: "CREATE INDEX IF NOT EXISTS idx_usage_records_created_at ON usage_records (created_at DESC) WHERE deleted_at IS NULL",
    },
    UsageIndexDefinition {
        table: "usage_records",
        name: "idx_usage_records_soft_cleanup_created",
        sql: "CREATE INDEX IF NOT EXISTS idx_usage_records_soft_cleanup_created ON usage_records (created_at ASC, id ASC) WHERE deleted_at IS NULL",
    },
    UsageIndexDefinition {
        table: "usage_records",
        name: "idx_usage_records_rollup_active_created_at",
        sql: "CREATE INDEX IF NOT EXISTS idx_usage_records_rollup_active_created_at ON usage_records (created_at DESC) WHERE deleted_at IS NULL AND rollup_active",
    },
    UsageIndexDefinition {
        table: "usage_records",
        name: "idx_usage_records_credential_created",
        sql: "CREATE INDEX IF NOT EXISTS idx_usage_records_credential_created ON usage_records (credential_id, created_at DESC) WHERE deleted_at IS NULL",
    },
    UsageIndexDefinition {
        table: "usage_records",
        name: "idx_usage_records_model_created",
        sql: "CREATE INDEX IF NOT EXISTS idx_usage_records_model_created ON usage_records (model, created_at DESC) WHERE deleted_at IS NULL",
    },
    UsageIndexDefinition {
        table: "usage_records",
        name: "idx_usage_records_upstream_model_created",
        sql: "CREATE INDEX IF NOT EXISTS idx_usage_records_upstream_model_created ON usage_records ((data->>'upstreamModel'), created_at DESC) WHERE deleted_at IS NULL",
    },
    UsageIndexDefinition {
        table: "usage_records",
        name: "idx_usage_records_external_outbound_model_created",
        sql: "CREATE INDEX IF NOT EXISTS idx_usage_records_external_outbound_model_created ON usage_records ((data->>'externalOutboundModel'), created_at DESC) WHERE deleted_at IS NULL",
    },
    UsageIndexDefinition {
        table: "usage_records",
        name: "idx_usage_records_external_pool_created",
        sql: "CREATE INDEX IF NOT EXISTS idx_usage_records_external_pool_created ON usage_records ((data->>'externalPoolId'), created_at DESC) WHERE deleted_at IS NULL",
    },
    UsageIndexDefinition {
        table: "usage_records",
        name: "idx_usage_records_status_created",
        sql: "CREATE INDEX IF NOT EXISTS idx_usage_records_status_created ON usage_records (status, created_at DESC) WHERE deleted_at IS NULL",
    },
    UsageIndexDefinition {
        table: "usage_records",
        name: "idx_usage_records_conversation",
        sql: "CREATE INDEX IF NOT EXISTS idx_usage_records_conversation ON usage_records (conversation_id) WHERE deleted_at IS NULL",
    },
    UsageIndexDefinition {
        table: "usage_records",
        name: "idx_usage_records_request_api_key_created",
        sql: "CREATE INDEX IF NOT EXISTS idx_usage_records_request_api_key_created ON usage_records ((data->>'requestApiKeyId'), created_at DESC) WHERE deleted_at IS NULL",
    },
    UsageIndexDefinition {
        table: "usage_records",
        name: "idx_usage_records_deleted_at",
        sql: "CREATE INDEX IF NOT EXISTS idx_usage_records_deleted_at ON usage_records (deleted_at ASC, id ASC) WHERE deleted_at IS NOT NULL",
    },
    UsageIndexDefinition {
        table: "usage_rollup_totals",
        name: "idx_usage_rollup_totals_dimension_cost",
        sql: "CREATE INDEX IF NOT EXISTS idx_usage_rollup_totals_dimension_cost ON usage_rollup_totals (dimension, total_estimated_cost_usd DESC, requests DESC)",
    },
    UsageIndexDefinition {
        table: "usage_rollup_time_buckets",
        name: "idx_usage_rollup_time_dimension_bucket",
        sql: "CREATE INDEX IF NOT EXISTS idx_usage_rollup_time_dimension_bucket ON usage_rollup_time_buckets (dimension, bucket_start)",
    },
    UsageIndexDefinition {
        table: "usage_rollup_time_buckets",
        name: "idx_usage_rollup_time_dimension_key_bucket",
        sql: "CREATE INDEX IF NOT EXISTS idx_usage_rollup_time_dimension_key_bucket ON usage_rollup_time_buckets (dimension, dimension_key, bucket_start)",
    },
    UsageIndexDefinition {
        table: "usage_cache_read_rollup_time_buckets",
        name: "idx_usage_cache_read_rollup_time_bucket",
        sql: "CREATE INDEX IF NOT EXISTS idx_usage_cache_read_rollup_time_bucket ON usage_cache_read_rollup_time_buckets (bucket_start, cache_read_input_tokens)",
    },
    UsageIndexDefinition {
        table: "usage_duration_rollup_time_buckets",
        name: "idx_usage_duration_rollup_time_bucket",
        sql: "CREATE INDEX IF NOT EXISTS idx_usage_duration_rollup_time_bucket ON usage_duration_rollup_time_buckets (bucket_start, duration_ms)",
    },
    UsageIndexDefinition {
        table: "usage_duration_rollup_time_buckets",
        name: "idx_usage_duration_rollup_positive_max",
        sql: "CREATE INDEX IF NOT EXISTS idx_usage_duration_rollup_positive_max ON usage_duration_rollup_time_buckets (duration_ms DESC) WHERE requests > 0",
    },
    UsageIndexDefinition {
        table: "usage_credential_cost_summary",
        name: "idx_usage_credential_cost_summary_cost",
        sql: "CREATE INDEX IF NOT EXISTS idx_usage_credential_cost_summary_cost ON usage_credential_cost_summary (estimated_cost_usd DESC, requests DESC)",
    },
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyResourceRow {
    pub id: u64,
    pub name: String,
    pub proxy_url: String,
    pub proxy_username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_password: Option<String>,
    pub enabled: bool,
    pub notes: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub credential_count: u64,
}

#[derive(Debug, Clone)]
pub struct CreateProxyResourceRow {
    pub name: String,
    pub proxy_url: String,
    pub proxy_username: Option<String>,
    pub proxy_password: Option<String>,
    pub enabled: bool,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct UpdateProxyResourceRow {
    pub name: Option<String>,
    pub proxy_url: Option<String>,
    pub proxy_username: Option<Option<String>>,
    pub proxy_password: Option<Option<String>>,
    pub enabled: Option<bool>,
    pub notes: Option<Option<String>>,
}

fn proxy_resource_from_row(row: PgRow) -> anyhow::Result<ProxyResourceRow> {
    let id: i64 = row.try_get("id")?;
    let created_at: DateTime<Utc> = row.try_get("created_at")?;
    let updated_at: DateTime<Utc> = row.try_get("updated_at")?;
    let credential_count: i64 = row.try_get("credential_count").unwrap_or(0);
    Ok(ProxyResourceRow {
        id: id as u64,
        name: row.try_get("name")?,
        proxy_url: row.try_get("proxy_url")?,
        proxy_username: row.try_get("proxy_username")?,
        proxy_password: row.try_get("proxy_password")?,
        enabled: row.try_get("enabled")?,
        notes: row.try_get("notes")?,
        created_at: created_at.to_rfc3339(),
        updated_at: updated_at.to_rfc3339(),
        credential_count: credential_count.max(0) as u64,
    })
}

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn credential_hash_columns(
    credential: &KiroCredentials,
) -> (String, Option<String>, Option<String>) {
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
        .then(|| credential.kiro_api_key.as_deref())
        .flatten()
        .filter(|value| !value.is_empty())
        .map(sha256_hex);
    let refresh_token_hash = (!is_api_key)
        .then(|| credential.refresh_token.as_deref())
        .flatten()
        .filter(|value| !value.is_empty())
        .map(sha256_hex);
    (auth_kind, api_key_hash, refresh_token_hash)
}

fn duplicate_credential_message(err: sqlx::Error) -> anyhow::Error {
    match &err {
        sqlx::Error::Database(db_err) if db_err.is_unique_violation() => {
            let constraint = db_err.constraint().unwrap_or_default();
            if constraint.contains("api_key") {
                return anyhow::anyhow!("凭据已存在（kiroApiKey 重复）");
            }
            if constraint.contains("refresh_token") {
                return anyhow::anyhow!("凭据已存在（refreshToken 重复）");
            }
            return anyhow::anyhow!("凭据已存在（唯一约束冲突）");
        }
        _ => {}
    }
    anyhow::Error::new(err)
}

fn credential_from_row(row: PgRow) -> anyhow::Result<KiroCredentials> {
    let id: i64 = row.try_get("id")?;
    let priority: i32 = row.try_get("priority")?;
    let disabled: bool = row.try_get("disabled")?;
    let created_at: DateTime<Utc> = row.try_get("created_at")?;
    let updated_at: DateTime<Utc> = row.try_get("updated_at")?;
    let revision: i64 = row.try_get("revision")?;
    let value: serde_json::Value = row.try_get("data")?;
    let mut credential: KiroCredentials = serde_json::from_value(value)?;
    credential.id = Some(id as u64);
    credential.created_at = Some(created_at.to_rfc3339());
    credential.updated_at = Some(updated_at.to_rfc3339());
    credential.storage_revision = revision.max(0) as u64;
    credential.priority = priority.max(0) as u32;
    credential.disabled = disabled;
    credential.canonicalize_auth_method();
    credential.normalize_supported_models();
    credential.normalize_api_key_defaults();
    credential.normalize_external_idp_defaults();
    Ok(credential)
}

fn credentials_from_rows(rows: Vec<PgRow>) -> anyhow::Result<Vec<KiroCredentials>> {
    rows.into_iter().map(credential_from_row).collect()
}

fn credential_runtime_states_from_rows(
    rows: Vec<PgRow>,
) -> anyhow::Result<HashMap<u64, CredentialRuntimeStateRow>> {
    let mut states = HashMap::with_capacity(rows.len());
    for row in rows {
        let credential_id: i64 = row.try_get("credential_id")?;
        states.insert(credential_id.max(0) as u64, runtime_state_from_row(&row)?);
    }
    Ok(states)
}

#[derive(Debug, Clone)]
pub struct PostgresStore {
    pool: PgPool,
    #[cfg(test)]
    test_schema: Option<String>,
}

pub struct PostgresUsageLifecycleGuard {
    connection: Option<PgConnection>,
    exclusive: bool,
}

impl PostgresUsageLifecycleGuard {
    pub async fn acquire_service(config: &Config) -> anyhow::Result<Self> {
        Self::acquire(config, false).await
    }

    pub async fn acquire_offline_maintenance(config: &Config) -> anyhow::Result<Self> {
        Self::acquire(config, true).await
    }

    async fn acquire(config: &Config, exclusive: bool) -> anyhow::Result<Self> {
        let url = config
            .postgres
            .url
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("必须配置 postgres.url"))?;
        let mut connection = PgConnection::connect(url).await?;
        let lock_sql = if exclusive {
            "SELECT pg_try_advisory_lock($1)"
        } else {
            "SELECT pg_try_advisory_lock_shared($1)"
        };
        let acquired: bool = sqlx::query_scalar(lock_sql)
            .bind(USAGE_OFFLINE_MAINTENANCE_LOCK_ID)
            .fetch_one(&mut connection)
            .await?;
        if !acquired {
            connection.close().await?;
            if exclusive {
                anyhow::bail!(
                    "usage maintenance requires every gateway instance to be stopped and its usage writer drained; an active current-version service still holds the PostgreSQL runtime fence"
                );
            }
            anyhow::bail!(
                "the gateway cannot start while offline usage maintenance holds the PostgreSQL runtime fence"
            );
        }
        Ok(Self {
            connection: Some(connection),
            exclusive,
        })
    }

    pub async fn release(mut self) -> anyhow::Result<()> {
        let Some(mut connection) = self.connection.take() else {
            return Ok(());
        };
        let unlock_sql = if self.exclusive {
            "SELECT pg_advisory_unlock($1)"
        } else {
            "SELECT pg_advisory_unlock_shared($1)"
        };
        let unlock_result = sqlx::query_scalar(unlock_sql)
            .bind(USAGE_OFFLINE_MAINTENANCE_LOCK_ID)
            .fetch_one(&mut connection)
            .await;
        let close_result = connection.close().await;
        let unlocked: bool = unlock_result?;
        close_result?;
        if !unlocked {
            anyhow::bail!("PostgreSQL usage lifecycle fence was not held during release");
        }
        Ok(())
    }
}

impl Drop for PostgresUsageLifecycleGuard {
    fn drop(&mut self) {
        let Some(connection) = self.connection.take() else {
            return;
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = connection.close_hard().await;
            });
        }
    }
}

#[cfg(test)]
fn postgres_drop_schema_error_is_retryable(error: &sqlx::Error) -> bool {
    error.as_database_error().is_some_and(|database_error| {
        database_error
            .code()
            .is_some_and(|code| matches!(code.as_ref(), "40P01" | "55P03"))
    })
}

impl PostgresStore {
    pub async fn connect(config: &Config) -> anyhow::Result<Self> {
        if config.postgres.compress_usage_rollups_on_start {
            anyhow::bail!(
                "postgres.compressUsageRollupsOnStart is no longer supported because online compression can drop blocked usage writes; stop and drain every service instance, then run `maintenance usage-rollup-compression`"
            );
        }
        let url = config
            .postgres
            .url
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("必须配置 postgres.url"))?;
        let pool = PgPoolOptions::new()
            .max_connections(config.postgres.max_connections.max(1))
            .connect(url)
            .await?;
        let store = Self {
            pool,
            #[cfg(test)]
            test_schema: None,
        };
        if config.postgres.migrate_on_start {
            store.migrate_with_options(false).await?;
        }
        store
            .verify_required_schema_compatibility(config.postgres.migrate_on_start)
            .await?;
        Ok(store)
    }

    pub async fn connect_usage(config: &Config) -> anyhow::Result<Self> {
        let url = config
            .postgres
            .url
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("必须配置 postgres.url"))?;
        let pool = PgPoolOptions::new()
            .max_connections(config.postgres.usage_max_connections.max(1))
            .connect(url)
            .await?;
        let store = Self {
            pool,
            #[cfg(test)]
            test_schema: None,
        };
        store.verify_required_schema_compatibility(false).await?;
        Ok(store)
    }

    #[cfg(test)]
    pub async fn connect_test(config: &Config) -> anyhow::Result<Self> {
        let url = config
            .postgres
            .url
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("必须配置 postgres.url"))?;
        let schema = format!("kiro_rs_test_{}", uuid::Uuid::new_v4().simple());
        let bootstrap_pool = PgPoolOptions::new().max_connections(1).connect(url).await?;
        sqlx::query(&format!(r#"CREATE SCHEMA "{}""#, schema))
            .execute(&bootstrap_pool)
            .await?;
        bootstrap_pool.close().await;

        let schema_for_connect = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(config.postgres.max_connections.max(1))
            .after_connect(move |conn, _meta| {
                let schema = schema_for_connect.clone();
                Box::pin(async move {
                    sqlx::query(&format!(r#"SET search_path TO "{}""#, schema))
                        .execute(conn)
                        .await?;
                    Ok(())
                })
            })
            .connect(url)
            .await?;
        let store = Self {
            pool,
            test_schema: Some(schema),
        };
        store.migrate_with_options(false).await?;
        store.verify_required_schema_compatibility(true).await?;
        Ok(store)
    }

    #[cfg(test)]
    pub(crate) async fn connect_test_peer(
        config: &Config,
        owner: &PostgresStore,
    ) -> anyhow::Result<Self> {
        let url = config
            .postgres
            .url
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("必须配置 postgres.url"))?;
        let schema = owner
            .test_schema
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("PostgreSQL test peer requires a test schema owner"))?
            .clone();
        let schema_for_connect = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(config.postgres.max_connections.max(1))
            .after_connect(move |conn, _meta| {
                let schema = schema_for_connect.clone();
                Box::pin(async move {
                    sqlx::query(&format!(r#"SET search_path TO "{}""#, schema))
                        .execute(conn)
                        .await?;
                    Ok(())
                })
            })
            .connect(url)
            .await?;
        Ok(Self {
            pool,
            test_schema: None,
        })
    }

    #[cfg(test)]
    pub(crate) async fn connect_usage_test_peer(
        config: &Config,
        owner: &PostgresStore,
    ) -> anyhow::Result<Self> {
        let url = config
            .postgres
            .url
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("必须配置 postgres.url"))?;
        let schema = owner
            .test_schema
            .as_ref()
            .ok_or_else(|| {
                anyhow::anyhow!("PostgreSQL usage test peer requires a test schema owner")
            })?
            .clone();
        let schema_for_connect = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(config.postgres.usage_max_connections.max(1))
            .after_connect(move |conn, _meta| {
                let schema = schema_for_connect.clone();
                Box::pin(async move {
                    sqlx::query(&format!(r#"SET search_path TO "{}""#, schema))
                        .execute(conn)
                        .await?;
                    Ok(())
                })
            })
            .connect(url)
            .await?;
        Ok(Self {
            pool,
            test_schema: None,
        })
    }

    async fn verify_required_schema_compatibility(
        &self,
        migrate_on_start: bool,
    ) -> anyhow::Result<()> {
        let mut table_names: Vec<String> = REQUIRED_POSTGRES_SCHEMA_COLUMNS
            .iter()
            .map(|required| required.table_name.to_string())
            .collect();
        table_names.sort();
        table_names.dedup();

        let mut column_names: Vec<String> = REQUIRED_POSTGRES_SCHEMA_COLUMNS
            .iter()
            .map(|required| required.column_name.to_string())
            .collect();
        column_names.sort();
        column_names.dedup();

        let rows = sqlx::query(
            r#"
            SELECT table_name, column_name
            FROM information_schema.columns
            WHERE table_schema = current_schema()
              AND table_name = ANY($1)
              AND column_name = ANY($2)
            "#,
        )
        .bind(&table_names)
        .bind(&column_names)
        .fetch_all(&self.pool)
        .await
        .map_err(|err| {
            anyhow::anyhow!("failed to verify PostgreSQL schema compatibility: {err}")
        })?;

        let present: HashSet<(String, String)> = rows
            .into_iter()
            .map(|row| {
                (
                    row.get::<String, _>("table_name"),
                    row.get::<String, _>("column_name"),
                )
            })
            .collect();

        let mut missing = required_postgres_schema_missing_columns(&present);
        if !self
            .usage_cleanup_batch_size_constraint_is_current()
            .await?
        {
            missing.push("usage_cleanup_jobs.batch_size_check<=5000".to_string());
        }
        if missing.is_empty() {
            return Ok(());
        }

        let migration_state = if migrate_on_start {
            "startup migration was enabled but did not produce the required schema"
        } else {
            "startup migration is disabled"
        };
        anyhow::bail!(
            "PostgreSQL schema is not compatible with this kiro.rs binary; missing required tables/columns: {}. {}. Set KIRO_RS_POSTGRES_MIGRATE_ON_START=true or postgres.migrateOnStart=true, restart once to migrate the database, and do not run the service with an old or partial schema.",
            missing.join(", "),
            migration_state
        );
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    async fn usage_cleanup_batch_size_constraint_is_current(&self) -> anyhow::Result<bool> {
        sqlx::query_scalar(
            r#"
            SELECT COALESCE(bool_or(pg_get_constraintdef(c.oid) LIKE '%<= 5000%'), false)
            FROM pg_constraint c
            JOIN pg_class t ON t.oid = c.conrelid
            JOIN pg_namespace n ON n.oid = t.relnamespace
            WHERE n.nspname = current_schema()
              AND t.relname = 'usage_cleanup_jobs'
              AND c.conname = 'usage_cleanup_jobs_batch_size_check'
            "#,
        )
        .fetch_one(&self.pool)
        .await
        .map_err(Into::into)
    }

    pub async fn ping(&self) -> anyhow::Result<()> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    #[cfg(test)]
    pub async fn drop_test_schema(&self) -> anyhow::Result<()> {
        let Some(schema) = &self.test_schema else {
            self.pool.close().await;
            return Ok(());
        };
        let query = format!(r#"DROP SCHEMA IF EXISTS "{}" CASCADE"#, schema);
        let mut delay = std::time::Duration::from_millis(25);
        let mut result = Ok(());
        for attempt in 0..5 {
            match sqlx::query(&query).execute(&self.pool).await {
                Ok(_) => break,
                Err(error) if postgres_drop_schema_error_is_retryable(&error) && attempt < 4 => {
                    tokio::time::sleep(delay).await;
                    delay = delay.saturating_mul(2);
                }
                Err(error) => {
                    result = Err(error.into());
                    break;
                }
            }
        }
        self.pool.close().await;
        result
    }

    pub async fn migrate_with_options(&self, compress_usage_rollups: bool) -> anyhow::Result<()> {
        let mut conn = self.pool.acquire().await?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(POSTGRES_MIGRATION_LOCK_ID)
            .execute(&mut *conn)
            .await?;

        let migration_result = async {
            let mut tx = conn.begin().await?;
            let startup_result = async {
                sqlx::query(
                    r#"
                    CREATE TABLE IF NOT EXISTS schema_migrations (
                        version TEXT PRIMARY KEY,
                        checksum TEXT NOT NULL,
                        applied_at TIMESTAMPTZ NOT NULL DEFAULT now()
                    )
                    "#,
                )
                .execute(&mut *tx)
                .await?;

                // Keep the default startup path bounded: schema expansion, small authority-table
                // repair, and explicitly versioned non-usage migrations only. Historical
                // usage_records backfills and rollup rebuilds must stay behind explicit
                // maintenance commands, otherwise a normal docker compose upgrade can block on
                // multi-GB production usage history before the service becomes available.
                execute_sql_statements_in_tx(&mut tx, SCHEMA_SQL).await?;
                ensure_usage_indexes_if_startup_safe(&mut tx).await?;
                repair_active_credential_hashes_in_tx(&mut tx).await?;
                run_versioned_migration_in_tx(
                    &mut tx,
                    "credential-storage-revision-v1",
                    CREDENTIAL_STORAGE_REVISION_SQL,
                )
                .await?;
                run_versioned_migration_in_tx(
                    &mut tx,
                    "credential-runtime-revision-v1",
                    CREDENTIAL_RUNTIME_REVISION_SQL,
                )
                .await?;
                run_versioned_migration_in_tx(
                    &mut tx,
                    "credential-runtime-generation-v1",
                    CREDENTIAL_RUNTIME_GENERATION_SQL,
                )
                .await?;
                run_versioned_migration_in_tx(
                    &mut tx,
                    "credential-runtime-mutation-cleanup-v1",
                    CREDENTIAL_RUNTIME_MUTATION_CLEANUP_SQL,
                )
                .await?;
                run_versioned_migration_in_tx(
                    &mut tx,
                    "credential-stats-delta-batches-v1",
                    CREDENTIAL_STATS_DELTA_BATCH_SQL,
                )
                .await?;
                run_versioned_migration_in_tx(
                    &mut tx,
                    "usage-cleanup-batch-size-limit-v1",
                    USAGE_CLEANUP_BATCH_SIZE_LIMIT_SQL,
                )
                .await?;

                sqlx::query(
                    r#"
                    INSERT INTO schema_migrations (version, checksum, applied_at)
                    VALUES ('inline-schema', $1, now())
                    ON CONFLICT (version) DO UPDATE
                    SET checksum = EXCLUDED.checksum,
                        applied_at = now()
                    WHERE schema_migrations.checksum IS DISTINCT FROM EXCLUDED.checksum
                    "#,
                )
                .bind(sha256_hex(SCHEMA_SQL))
                .execute(&mut *tx)
                .await?;
                Ok::<(), anyhow::Error>(())
            }
            .await;

            match startup_result {
                Ok(()) => tx.commit().await?,
                Err(error) => {
                    if let Err(rollback_error) = tx.rollback().await {
                        anyhow::bail!(
                            "startup migration failed: {error:#}; rollback failed: {rollback_error}"
                        );
                    }
                    return Err(error);
                }
            }

            // Compression is an explicit, potentially large maintenance operation. Keep it out
            // of the bounded default startup transaction while retaining the session lock.
            if compress_usage_rollups {
                run_usage_exclusive_versioned_migration(
                    &mut conn,
                    "usage-rollup-hour-bucket-compression-v1",
                    USAGE_ROLLUP_HOUR_BUCKET_COMPRESSION_SQL,
                )
                .await?;
            }
            Ok::<(), anyhow::Error>(())
        }
        .await;

        let unlock_result = sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(POSTGRES_MIGRATION_LOCK_ID)
            .execute(&mut *conn)
            .await;
        migration_result?;
        unlock_result?;
        Ok(())
    }

    pub async fn compress_usage_rollups_to_hour_buckets(&self) -> anyhow::Result<()> {
        self.migrate_with_options(true).await
    }

    pub async fn create_usage_indexes_concurrently(&self) -> anyhow::Result<()> {
        let mut conn = self.pool.acquire().await?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(POSTGRES_MIGRATION_LOCK_ID)
            .execute(&mut *conn)
            .await?;

        let result = async {
            for definition in USAGE_INDEX_DEFINITIONS {
                let sql = definition.sql.replacen(
                    "CREATE INDEX IF NOT EXISTS",
                    "CREATE INDEX CONCURRENTLY IF NOT EXISTS",
                    1,
                );
                sqlx::query(&sql).execute(&mut *conn).await?;
            }
            Ok::<(), anyhow::Error>(())
        }
        .await;

        let unlock_result = sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(POSTGRES_MIGRATION_LOCK_ID)
            .execute(&mut *conn)
            .await;
        result?;
        unlock_result?;
        Ok(())
    }

    pub async fn backfill_usage_legacy_cost_fields(&self) -> anyhow::Result<()> {
        let mut conn = self.pool.acquire().await?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(POSTGRES_MIGRATION_LOCK_ID)
            .execute(&mut *conn)
            .await?;

        let result = async {
            sqlx::query(
                r#"
                CREATE TABLE IF NOT EXISTS schema_migrations (
                    version TEXT PRIMARY KEY,
                    checksum TEXT NOT NULL,
                    applied_at TIMESTAMPTZ NOT NULL DEFAULT now()
                )
                "#,
            )
            .execute(&mut *conn)
            .await?;
            execute_sql_statements(&mut conn, SCHEMA_SQL).await?;
            run_usage_exclusive_versioned_migration(
                &mut conn,
                "usage-legacy-cost-field-backfill-v1",
                USAGE_LEGACY_COST_FIELD_BACKFILL_SQL,
            )
            .await?;
            Ok::<(), anyhow::Error>(())
        }
        .await;

        let unlock_result = sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(POSTGRES_MIGRATION_LOCK_ID)
            .execute(&mut *conn)
            .await;
        result?;
        unlock_result?;
        Ok(())
    }

    pub async fn load_runtime_config(&self) -> anyhow::Result<Option<Config>> {
        let row = sqlx::query("SELECT config FROM runtime_config WHERE id = 'default'")
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let value: serde_json::Value = row.try_get("config")?;
        let mut config: Config = serde_json::from_value(value)?;
        config.set_config_path_for_runtime(None);
        Ok(Some(config))
    }

    pub async fn save_runtime_config(&self, config: &Config) -> anyhow::Result<()> {
        self.save_runtime_config_returning_version(config).await?;
        Ok(())
    }

    pub async fn save_runtime_config_returning_version(
        &self,
        config: &Config,
    ) -> anyhow::Result<i64> {
        let value = serde_json::to_value(config)?;
        let row = sqlx::query(
            r#"
            INSERT INTO runtime_config (id, config, updated_at)
            VALUES ('default', $1, now())
            ON CONFLICT (id) DO UPDATE
            SET config = EXCLUDED.config,
                version = runtime_config.version + 1,
                updated_at = now()
            RETURNING version
            "#,
        )
        .bind(value)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("version")?)
    }

    pub async fn bootstrap_runtime_config_from_file(
        &self,
        file_config: &Config,
    ) -> anyhow::Result<()> {
        match self.load_runtime_config().await? {
            None => {
                let mut config = file_config.clone();
                config.set_request_api_keys(file_config.request_api_keys());
                config.apply_runtime_config_migrations();
                self.save_runtime_config(&config).await?;
            }
            Some(mut config) => {
                let mut changed = config.fill_missing_access_keys_from(file_config);
                changed |= config.apply_runtime_config_migrations();
                if changed {
                    self.save_runtime_config(&config).await?;
                }
            }
        }
        Ok(())
    }

    pub async fn list_external_pools(
        &self,
        mask_secrets: bool,
    ) -> anyhow::Result<Vec<ExternalPool>> {
        let rows = sqlx::query(EXTERNAL_POOL_SELECT_SQL)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| external_pool_from_row(row, mask_secrets))
            .collect()
    }

    /// Loads only rows that can be decoded without compatibility defaults.
    /// A malformed pool is isolated from healthy pools and cannot authorize dispatch.
    pub async fn list_dispatchable_external_pools(&self) -> anyhow::Result<Vec<ExternalPool>> {
        let rows = sqlx::query(EXTERNAL_POOL_SELECT_SQL)
            .fetch_all(&self.pool)
            .await?;
        let mut pools = Vec::with_capacity(rows.len());
        let mut invalid_pool_ids = Vec::new();
        let mut invalid_rows = 0usize;
        for row in rows {
            let pool_id = row.try_get::<i64, _>("id").ok();
            match external_pool_from_row_with_policy(row, false, true) {
                Ok(pool) => pools.push(pool),
                Err(_) => {
                    invalid_rows = invalid_rows.saturating_add(1);
                    if invalid_pool_ids.len() < 16 {
                        invalid_pool_ids.extend(pool_id);
                    }
                }
            }
        }
        if invalid_rows > 0 {
            tracing::debug!(
                invalid_rows,
                invalid_pool_ids = ?invalid_pool_ids,
                "外部池持久化配置无法安全解析，调度资格按池 fail closed"
            );
        }
        Ok(pools)
    }

    pub async fn list_external_pool_eligibility(
        &self,
    ) -> anyhow::Result<Vec<ExternalPoolEligibility>> {
        let rows = sqlx::query(
            r#"
            SELECT id, revision, enabled, base_url,
                   (btrim(api_key) <> '') AS api_key_present,
                   auth_type, max_concurrent_requests, usage_projection_mode,
                   stream_response_mode, request_body_mode, raw_model_mode,
                   auto_disable_policy, pre_output_stream_retry_mode,
                   header_profile, append_beta_query, header_overrides,
                   wire_profile, tls_profile, model_mapping_mode,
                   model_mapping_require_match, model_mapping_rules,
                   auto_disabled, auto_disabled_until, supported_models,
                   route_mode, route_rules
            FROM external_upstream_pools
            WHERE deleted_at IS NULL
            ORDER BY priority ASC, id ASC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        let mut eligibility = Vec::with_capacity(rows.len());
        let mut invalid_pool_ids = Vec::new();
        let mut invalid_reasons = Vec::new();
        let mut invalid_rows = 0usize;
        for row in rows {
            let pool_id = row.try_get::<i64, _>("id").ok();
            match external_pool_eligibility_from_row(&row) {
                Ok(pool) => eligibility.push(pool),
                Err(err) => {
                    invalid_rows = invalid_rows.saturating_add(1);
                    if invalid_pool_ids.len() < 16 {
                        invalid_pool_ids.extend(pool_id);
                    }
                    if invalid_reasons.len() < 4 {
                        invalid_reasons.push(err.to_string());
                    }
                }
            }
        }
        if invalid_rows > 0 {
            tracing::warn!(
                invalid_rows,
                invalid_pool_ids = ?invalid_pool_ids,
                invalid_reasons = ?invalid_reasons,
                "外部池静态资格配置无法安全解析，按池 fail closed"
            );
        }
        Ok(eligibility)
    }

    pub async fn get_external_pool(
        &self,
        id: u64,
        mask_secrets: bool,
    ) -> anyhow::Result<Option<ExternalPool>> {
        let row = sqlx::query(
            r#"
            SELECT id, name, base_url, api_key, auth_type, enabled, priority,
                   max_concurrent_requests, usage_projection_mode, stream_response_mode,
                   request_body_mode, raw_model_mode, auto_disable_policy, pre_output_stream_retry_mode,
                   header_profile, append_beta_query, header_overrides,
                   wire_profile, tls_profile,
                   auto_disabled, auto_disabled_reason, auto_disabled_at,
                   auto_disabled_until, auto_disabled_last_error, preserve_path,
                   normalize_model_version_dots, model_mapping_mode,
                   model_mapping_require_match, model_mapping_rules, supported_models,
                   route_mode, route_rules, notes,
                   revision, created_at, updated_at
            FROM external_upstream_pools
            WHERE id = $1 AND deleted_at IS NULL
            "#,
        )
        .bind(id as i64)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| external_pool_from_row(row, mask_secrets))
            .transpose()
    }

    /// Defines the request's dispatch linearization point after request preparation and before
    /// attempt reservation/HTTP send. A mutation committed after this query does not revoke an
    /// already-started dispatch; callers must not hold a PostgreSQL lock across upstream I/O.
    pub async fn external_pool_dispatch_revision_matches(
        &self,
        id: u64,
        expected_revision: u64,
    ) -> anyhow::Result<bool> {
        let matches: bool = sqlx::query_scalar(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM external_upstream_pools
                WHERE id = $1
                  AND revision = $2
                  AND deleted_at IS NULL
                  AND enabled = true
                  AND (
                      auto_disabled = false
                      OR (auto_disabled_until IS NOT NULL AND auto_disabled_until <= now())
                  )
            )
            "#,
        )
        .bind(id as i64)
        .bind(
            i64::try_from(expected_revision)
                .map_err(|_| anyhow::anyhow!("外部池 revision 超出 PgSQL BIGINT 范围"))?,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(matches)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn create_external_pool(
        &self,
        request: CreateExternalPoolRequest,
    ) -> anyhow::Result<ExternalPool> {
        self.create_external_pool_with_secret_policy(request, true)
            .await
    }

    pub(crate) async fn create_external_pool_unmasked(
        &self,
        request: CreateExternalPoolRequest,
    ) -> anyhow::Result<ExternalPool> {
        self.create_external_pool_with_secret_policy(request, false)
            .await
    }

    async fn create_external_pool_with_secret_policy(
        &self,
        request: CreateExternalPoolRequest,
        mask_secrets: bool,
    ) -> anyhow::Result<ExternalPool> {
        validate_external_pool_input(
            &request.name,
            &request.base_url,
            request.max_concurrent_requests,
        )?;
        let model_mapping_rules =
            normalize_external_pool_model_mapping_rules(request.model_mapping_rules);
        let model_mapping_rules_value = serde_json::to_value(&model_mapping_rules)?;
        let supported_models = normalize_supported_models(request.supported_models);
        let supported_models_value = serde_json::to_value(&supported_models)?;
        validate_external_pool_route_rules(&request.route_rules)?;
        let route_rules = normalize_route_rules(&request.route_rules);
        let route_rules_value = serde_json::to_value(&route_rules)?;
        let header_overrides = normalize_external_pool_header_overrides(request.header_overrides)?;
        let header_overrides_value = serde_json::to_value(&header_overrides)?;
        let row = sqlx::query(
            r#"
            INSERT INTO external_upstream_pools (
                name, base_url, api_key, auth_type, enabled, priority,
                max_concurrent_requests, usage_projection_mode, stream_response_mode,
                request_body_mode, raw_model_mode, auto_disable_policy, pre_output_stream_retry_mode,
                header_profile, append_beta_query, header_overrides,
                wire_profile, tls_profile,
                preserve_path, normalize_model_version_dots, model_mapping_mode,
                model_mapping_require_match, model_mapping_rules, supported_models,
                route_mode, route_rules, notes, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24, $25, $26, $27, now())
            RETURNING id, name, base_url, api_key, auth_type, enabled, priority,
                      max_concurrent_requests, usage_projection_mode, stream_response_mode,
                      request_body_mode, raw_model_mode, auto_disable_policy, pre_output_stream_retry_mode,
                      header_profile, append_beta_query, header_overrides,
                      wire_profile, tls_profile,
                      auto_disabled, auto_disabled_reason, auto_disabled_at,
                      auto_disabled_until, auto_disabled_last_error, preserve_path,
                      normalize_model_version_dots, model_mapping_mode,
                      model_mapping_require_match, model_mapping_rules, supported_models,
                      route_mode, route_rules, notes,
                      revision, created_at, updated_at
            "#,
        )
        .bind(request.name.trim())
        .bind(request.base_url.trim().trim_end_matches('/'))
        .bind(request.api_key.trim())
        .bind(request.auth_type.as_str())
        .bind(request.enabled)
        .bind(request.priority)
        .bind(request.max_concurrent_requests as i32)
        .bind(request.usage_projection_mode.as_str())
        .bind(request.stream_response_mode.map(|mode| mode.as_str()))
        .bind(request.request_body_mode.as_str())
        .bind(request.raw_model_mode.as_str())
        .bind(request.auto_disable_policy.as_str())
        .bind(request.pre_output_stream_retry_mode.as_str())
        .bind(request.header_profile.as_str())
        .bind(request.append_beta_query)
        .bind(header_overrides_value)
        .bind(request.wire_profile.as_str())
        .bind(request.tls_profile.as_str())
        .bind(request.preserve_path)
        .bind(request.normalize_model_version_dots)
        .bind(request.model_mapping_mode.as_str())
        .bind(request.model_mapping_require_match)
        .bind(model_mapping_rules_value)
        .bind(supported_models_value)
        .bind(request.route_mode.as_str())
        .bind(route_rules_value)
        .bind(request.notes.map(|notes| notes.trim().to_string()))
        .fetch_one(&self.pool)
        .await?;
        external_pool_from_row(row, mask_secrets)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn update_external_pool(
        &self,
        id: u64,
        request: UpdateExternalPoolRequest,
    ) -> anyhow::Result<Option<ExternalPool>> {
        self.update_external_pool_with_secret_policy(id, request, true)
            .await
    }

    pub(crate) async fn update_external_pool_unmasked(
        &self,
        id: u64,
        request: UpdateExternalPoolRequest,
    ) -> anyhow::Result<Option<ExternalPool>> {
        self.update_external_pool_with_secret_policy(id, request, false)
            .await
    }

    async fn update_external_pool_with_secret_policy(
        &self,
        id: u64,
        request: UpdateExternalPoolRequest,
        mask_secrets: bool,
    ) -> anyhow::Result<Option<ExternalPool>> {
        let Some(current) = self.get_external_pool(id, false).await? else {
            return Ok(None);
        };
        let name = request.name.unwrap_or(current.name).trim().to_string();
        let base_url = request
            .base_url
            .unwrap_or(current.base_url)
            .trim()
            .trim_end_matches('/')
            .to_string();
        let api_key = request
            .api_key
            .filter(|value| !value.trim().is_empty())
            .or(current.api_key)
            .unwrap_or_default();
        let auth_type = request.auth_type.unwrap_or(current.auth_type);
        let enabled = request.enabled.unwrap_or(current.enabled);
        let priority = request.priority.unwrap_or(current.priority);
        let max_concurrent_requests = request
            .max_concurrent_requests
            .unwrap_or(current.max_concurrent_requests);
        let usage_projection_mode = request
            .usage_projection_mode
            .unwrap_or(current.usage_projection_mode);
        let stream_response_mode = request
            .stream_response_mode
            .unwrap_or(current.stream_response_mode);
        let request_body_mode = request
            .request_body_mode
            .unwrap_or(current.request_body_mode);
        let raw_model_mode = request.raw_model_mode.unwrap_or(current.raw_model_mode);
        let auto_disable_policy = request
            .auto_disable_policy
            .unwrap_or(current.auto_disable_policy);
        let pre_output_stream_retry_mode = request
            .pre_output_stream_retry_mode
            .unwrap_or(current.pre_output_stream_retry_mode);
        let header_profile = request.header_profile.unwrap_or(current.header_profile);
        let append_beta_query = request
            .append_beta_query
            .unwrap_or(current.append_beta_query);
        let header_overrides = request
            .header_overrides
            .map(normalize_external_pool_header_overrides)
            .transpose()?
            .unwrap_or(current.header_overrides);
        let header_overrides_value = serde_json::to_value(&header_overrides)?;
        let wire_profile = request.wire_profile.unwrap_or(current.wire_profile);
        let tls_profile = request.tls_profile.unwrap_or(current.tls_profile);
        let preserve_path = request.preserve_path.unwrap_or(current.preserve_path);
        let normalize_model_version_dots = request
            .normalize_model_version_dots
            .unwrap_or(current.normalize_model_version_dots);
        let model_mapping_mode = request
            .model_mapping_mode
            .unwrap_or(current.model_mapping_mode);
        let model_mapping_require_match = request
            .model_mapping_require_match
            .unwrap_or(current.model_mapping_require_match);
        let model_mapping_rules = request
            .model_mapping_rules
            .map(normalize_external_pool_model_mapping_rules)
            .unwrap_or(current.model_mapping_rules);
        let model_mapping_rules_value = serde_json::to_value(&model_mapping_rules)?;
        let supported_models = request
            .supported_models
            .map(normalize_supported_models)
            .unwrap_or(current.supported_models);
        let supported_models_value = serde_json::to_value(&supported_models)?;
        let route_mode = request.route_mode.unwrap_or(current.route_mode);
        let route_rules = request
            .route_rules
            .map(|rules| {
                validate_external_pool_route_rules(&rules)?;
                Ok::<_, anyhow::Error>(normalize_route_rules(&rules))
            })
            .transpose()?
            .unwrap_or(current.route_rules);
        let route_rules_value = serde_json::to_value(&route_rules)?;
        let notes = request.notes.or(current.notes);
        validate_external_pool_input(&name, &base_url, max_concurrent_requests)?;
        let row = sqlx::query(
            r#"
            UPDATE external_upstream_pools
            SET name = $2,
                base_url = $3,
                api_key = $4,
                auth_type = $5,
                enabled = $6,
                priority = $7,
                max_concurrent_requests = $8,
                usage_projection_mode = $9,
                stream_response_mode = $10,
                request_body_mode = $11,
                raw_model_mode = $12,
                auto_disable_policy = $13,
                pre_output_stream_retry_mode = $14,
                header_profile = $15,
                append_beta_query = $16,
                header_overrides = $17,
                wire_profile = $18,
                tls_profile = $19,
                preserve_path = $20,
                normalize_model_version_dots = $21,
                model_mapping_mode = $22,
                model_mapping_require_match = $23,
                model_mapping_rules = $24,
                supported_models = $25,
                route_mode = $26,
                route_rules = $27,
                notes = $28,
                revision = revision + 1,
                updated_at = now()
            WHERE id = $1 AND deleted_at IS NULL
            RETURNING id, name, base_url, api_key, auth_type, enabled, priority,
                      max_concurrent_requests, usage_projection_mode, stream_response_mode,
                      request_body_mode, raw_model_mode, auto_disable_policy, pre_output_stream_retry_mode,
                      header_profile, append_beta_query, header_overrides,
                      wire_profile, tls_profile,
                      auto_disabled, auto_disabled_reason, auto_disabled_at,
                      auto_disabled_until, auto_disabled_last_error, preserve_path,
                      normalize_model_version_dots, model_mapping_mode,
                      model_mapping_require_match, model_mapping_rules, supported_models,
                      route_mode, route_rules, notes,
                      revision, created_at, updated_at
            "#,
        )
        .bind(id as i64)
        .bind(name)
        .bind(base_url)
        .bind(api_key.trim())
        .bind(auth_type.as_str())
        .bind(enabled)
        .bind(priority)
        .bind(max_concurrent_requests as i32)
        .bind(usage_projection_mode.as_str())
        .bind(stream_response_mode.map(|mode| mode.as_str()))
        .bind(request_body_mode.as_str())
        .bind(raw_model_mode.as_str())
        .bind(auto_disable_policy.as_str())
        .bind(pre_output_stream_retry_mode.as_str())
        .bind(header_profile.as_str())
        .bind(append_beta_query)
        .bind(header_overrides_value)
        .bind(wire_profile.as_str())
        .bind(tls_profile.as_str())
        .bind(preserve_path)
        .bind(normalize_model_version_dots)
        .bind(model_mapping_mode.as_str())
        .bind(model_mapping_require_match)
        .bind(model_mapping_rules_value)
        .bind(supported_models_value)
        .bind(route_mode.as_str())
        .bind(route_rules_value)
        .bind(notes)
        .fetch_one(&self.pool)
        .await?;
        Ok(Some(external_pool_from_row(row, mask_secrets)?))
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn set_external_pool_enabled(
        &self,
        id: u64,
        enabled: bool,
    ) -> anyhow::Result<Option<ExternalPool>> {
        self.set_external_pool_enabled_with_secret_policy(id, enabled, true)
            .await
    }

    pub(crate) async fn set_external_pool_enabled_unmasked(
        &self,
        id: u64,
        enabled: bool,
    ) -> anyhow::Result<Option<ExternalPool>> {
        self.set_external_pool_enabled_with_secret_policy(id, enabled, false)
            .await
    }

    async fn set_external_pool_enabled_with_secret_policy(
        &self,
        id: u64,
        enabled: bool,
        mask_secrets: bool,
    ) -> anyhow::Result<Option<ExternalPool>> {
        let row = sqlx::query(
            r#"
            UPDATE external_upstream_pools
            SET enabled = $2,
                revision = revision + 1,
                updated_at = now()
            WHERE id = $1 AND deleted_at IS NULL
            RETURNING id, name, base_url, api_key, auth_type, enabled, priority,
                      max_concurrent_requests, usage_projection_mode, stream_response_mode,
                      request_body_mode, raw_model_mode, auto_disable_policy, pre_output_stream_retry_mode,
                      header_profile, append_beta_query, header_overrides,
                      wire_profile, tls_profile,
                      auto_disabled, auto_disabled_reason, auto_disabled_at,
                      auto_disabled_until, auto_disabled_last_error, preserve_path,
                      normalize_model_version_dots, model_mapping_mode,
                      model_mapping_require_match, model_mapping_rules, supported_models,
                      route_mode, route_rules, notes,
                      revision, created_at, updated_at
            "#,
        )
        .bind(id as i64)
        .bind(enabled)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| external_pool_from_row(row, mask_secrets))
            .transpose()
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn set_external_pool_supported_models(
        &self,
        id: u64,
        supported_models: Vec<String>,
    ) -> anyhow::Result<Option<ExternalPool>> {
        self.set_external_pool_supported_models_with_secret_policy(id, supported_models, true)
            .await
    }

    pub(crate) async fn set_external_pool_supported_models_unmasked(
        &self,
        id: u64,
        supported_models: Vec<String>,
    ) -> anyhow::Result<Option<ExternalPool>> {
        self.set_external_pool_supported_models_with_secret_policy(id, supported_models, false)
            .await
    }

    async fn set_external_pool_supported_models_with_secret_policy(
        &self,
        id: u64,
        supported_models: Vec<String>,
        mask_secrets: bool,
    ) -> anyhow::Result<Option<ExternalPool>> {
        let supported_models = normalize_supported_models(supported_models);
        let supported_models_value = serde_json::to_value(&supported_models)?;
        let row = sqlx::query(
            r#"
            UPDATE external_upstream_pools
            SET supported_models = $2,
                revision = revision + 1,
                updated_at = now()
            WHERE id = $1 AND deleted_at IS NULL
            RETURNING id, name, base_url, api_key, auth_type, enabled, priority,
                      max_concurrent_requests, usage_projection_mode, stream_response_mode,
                      request_body_mode, raw_model_mode, auto_disable_policy, pre_output_stream_retry_mode,
                      header_profile, append_beta_query, header_overrides,
                      wire_profile, tls_profile,
                      auto_disabled, auto_disabled_reason, auto_disabled_at,
                      auto_disabled_until, auto_disabled_last_error, preserve_path,
                      normalize_model_version_dots, model_mapping_mode,
                      model_mapping_require_match, model_mapping_rules, supported_models,
                      route_mode, route_rules, notes,
                      revision, created_at, updated_at
            "#,
        )
        .bind(id as i64)
        .bind(supported_models_value)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| external_pool_from_row(row, mask_secrets))
            .transpose()
    }

    pub async fn soft_delete_external_pool(&self, id: u64) -> anyhow::Result<bool> {
        let result = sqlx::query(
            "UPDATE external_upstream_pools SET deleted_at = now(), revision = revision + 1, updated_at = now() WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(id as i64)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn clear_external_pool_auto_disabled(
        &self,
        id: u64,
    ) -> anyhow::Result<Option<ExternalPool>> {
        self.clear_external_pool_auto_disabled_with_secret_policy(id, true)
            .await
    }

    pub(crate) async fn clear_external_pool_auto_disabled_unmasked(
        &self,
        id: u64,
    ) -> anyhow::Result<Option<ExternalPool>> {
        self.clear_external_pool_auto_disabled_with_secret_policy(id, false)
            .await
    }

    async fn clear_external_pool_auto_disabled_with_secret_policy(
        &self,
        id: u64,
        mask_secrets: bool,
    ) -> anyhow::Result<Option<ExternalPool>> {
        let row = sqlx::query(
            r#"
            UPDATE external_upstream_pools
            SET auto_disabled = false,
                auto_disabled_reason = NULL,
                auto_disabled_at = NULL,
                auto_disabled_until = NULL,
                auto_disabled_last_error = NULL,
                revision = revision + 1,
                updated_at = now()
            WHERE id = $1 AND deleted_at IS NULL
            RETURNING id, name, base_url, api_key, auth_type, enabled, priority,
                      max_concurrent_requests, usage_projection_mode, stream_response_mode,
                      request_body_mode, raw_model_mode, auto_disable_policy, pre_output_stream_retry_mode,
                      auto_disabled, auto_disabled_reason, auto_disabled_at,
                      auto_disabled_until, auto_disabled_last_error, preserve_path,
                      normalize_model_version_dots, model_mapping_mode,
                      model_mapping_require_match, model_mapping_rules, supported_models,
                      route_mode, route_rules, notes,
                      revision, created_at, updated_at
            "#,
        )
        .bind(id as i64)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| external_pool_from_row(row, mask_secrets))
            .transpose()
    }

    #[cfg(test)]
    pub async fn auto_disable_external_pool(
        &self,
        id: u64,
        reason: &str,
        last_error: &str,
        duration_secs: u64,
    ) -> anyhow::Result<()> {
        let Some(pool) = self.get_external_pool(id, true).await? else {
            return Ok(());
        };
        let _ = self
            .auto_disable_external_pool_if_unchanged(
                id,
                pool.revision,
                reason,
                last_error,
                duration_secs,
            )
            .await?;
        Ok(())
    }

    pub async fn auto_disable_external_pool_if_unchanged(
        &self,
        id: u64,
        expected_revision: u64,
        reason: &str,
        last_error: &str,
        duration_secs: u64,
    ) -> anyhow::Result<bool> {
        let until = if duration_secs == 0 {
            None
        } else {
            Some(Utc::now() + chrono::Duration::seconds(duration_secs as i64))
        };
        let result = sqlx::query(
            r#"
            UPDATE external_upstream_pools
            SET auto_disabled = true,
                auto_disabled_reason = $2,
                auto_disabled_at = now(),
                auto_disabled_until = $3,
                auto_disabled_last_error = $4,
                revision = revision + 1,
                updated_at = now()
            WHERE id = $1
              AND revision = $5
              AND deleted_at IS NULL
              AND (
                  auto_disabled = false
                  OR (auto_disabled_until IS NOT NULL AND auto_disabled_until <= now())
              )
            "#,
        )
        .bind(id as i64)
        .bind(reason)
        .bind(until)
        .bind(last_error)
        .bind(
            i64::try_from(expected_revision)
                .map_err(|_| anyhow::anyhow!("外部池 revision 超出 PgSQL BIGINT 范围"))?,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    #[cfg(test)]
    pub async fn load_credentials(&self) -> anyhow::Result<Vec<KiroCredentials>> {
        let rows = sqlx::query(ACTIVE_CREDENTIALS_SELECT_SQL)
            .fetch_all(&self.pool)
            .await?;

        credentials_from_rows(rows)
    }

    pub async fn load_credential(
        &self,
        credential_id: u64,
    ) -> anyhow::Result<Option<KiroCredentials>> {
        let credential_id_i64 = i64::try_from(credential_id)
            .map_err(|_| anyhow::anyhow!("凭据 id 超出 PgSQL BIGINT 范围"))?;
        let row = sqlx::query(
            r#"
            SELECT id, priority, disabled, data, created_at, updated_at, revision
            FROM credentials
            WHERE id = $1 AND deleted_at IS NULL
            "#,
        )
        .bind(credential_id_i64)
        .fetch_optional(&self.pool)
        .await?;

        row.map(credential_from_row).transpose()
    }

    #[cfg(test)]
    pub async fn load_credentials_with_runtime_state(
        &self,
    ) -> anyhow::Result<(
        Vec<KiroCredentials>,
        HashMap<u64, CredentialRuntimeStateRow>,
    )> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        let credential_rows = sqlx::query(ACTIVE_CREDENTIALS_SELECT_SQL)
            .fetch_all(&mut *tx)
            .await?;
        let runtime_state_rows = sqlx::query(CREDENTIAL_RUNTIME_STATE_SELECT_SQL)
            .fetch_all(&mut *tx)
            .await?;
        let credentials = credentials_from_rows(credential_rows)?;
        let runtime_states = credential_runtime_states_from_rows(runtime_state_rows)?;
        tx.commit().await?;

        Ok((credentials, runtime_states))
    }

    /// Load the credential configuration, authoritative runtime state, and the latest
    /// account-balance snapshot from one repeatable-read transaction.
    ///
    /// The account snapshot is intentionally returned separately from runtime state: it is
    /// display/probe data and must never overwrite an explicit Admin disable or runtime
    /// generation. Scheduler admission may derive a short-lived quota guard from it.
    pub async fn load_credentials_with_runtime_state_and_account_info(
        &self,
    ) -> anyhow::Result<(
        Vec<KiroCredentials>,
        HashMap<u64, CredentialRuntimeStateRow>,
        HashMap<u64, CredentialAccountInfoRow>,
    )> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        let credential_rows = sqlx::query(ACTIVE_CREDENTIALS_SELECT_SQL)
            .fetch_all(&mut *tx)
            .await?;
        let runtime_state_rows = sqlx::query(CREDENTIAL_RUNTIME_STATE_SELECT_SQL)
            .fetch_all(&mut *tx)
            .await?;
        let account_info_rows = sqlx::query(
            r#"
            SELECT credential_id, subscription_title, current_usage, usage_limit,
                   remaining, usage_percentage, credit_limit, credit_remaining,
                   credit_base, credit_bonus, overage_status, overage_capability,
                   overage_cap, overage_rate, current_overages, next_reset_at, checked_at
            FROM credential_account_info
            "#,
        )
        .fetch_all(&mut *tx)
        .await?;
        let credentials = credentials_from_rows(credential_rows)?;
        let runtime_states = credential_runtime_states_from_rows(runtime_state_rows)?;
        let mut account_info = HashMap::with_capacity(account_info_rows.len());
        for row in account_info_rows {
            let credential_id: i64 = row.try_get("credential_id")?;
            let checked_at: DateTime<Utc> = row.try_get("checked_at")?;
            account_info.insert(
                credential_id as u64,
                CredentialAccountInfoRow {
                    subscription_title: row.try_get("subscription_title")?,
                    current_usage: row.try_get("current_usage")?,
                    usage_limit: row.try_get("usage_limit")?,
                    remaining: row.try_get("remaining")?,
                    usage_percentage: row.try_get("usage_percentage")?,
                    credit_limit: row.try_get("credit_limit")?,
                    credit_remaining: row.try_get("credit_remaining")?,
                    credit_base: row.try_get("credit_base")?,
                    credit_bonus: row.try_get("credit_bonus")?,
                    overage_status: row.try_get("overage_status")?,
                    overage_capability: row.try_get("overage_capability")?,
                    overage_cap: row.try_get("overage_cap")?,
                    overage_rate: row.try_get("overage_rate")?,
                    current_overages: row.try_get("current_overages")?,
                    next_reset_at: row.try_get("next_reset_at")?,
                    checked_at: checked_at.to_rfc3339(),
                },
            );
        }
        tx.commit().await?;

        Ok((credentials, runtime_states, account_info))
    }

    pub async fn load_credential_with_runtime_state(
        &self,
        credential_id: u64,
    ) -> anyhow::Result<Option<(KiroCredentials, Option<CredentialRuntimeStateRow>)>> {
        let credential_id_i64 = i64::try_from(credential_id)
            .map_err(|_| anyhow::anyhow!("凭据 id 超出 PgSQL BIGINT 范围"))?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        let credential = sqlx::query(
            r#"
            SELECT id, priority, disabled, data, created_at, updated_at, revision
            FROM credentials
            WHERE id = $1 AND deleted_at IS NULL
            "#,
        )
        .bind(credential_id_i64)
        .fetch_optional(&mut *tx)
        .await?
        .map(credential_from_row)
        .transpose()?;
        let Some(credential) = credential else {
            tx.commit().await?;
            return Ok(None);
        };
        let runtime_state = sqlx::query(
            r#"
            SELECT credential_id, failure_count, refresh_failure_count,
                   disabled_reason, warmup_remaining, generation, revision
            FROM credential_runtime_state
            WHERE credential_id = $1
            "#,
        )
        .bind(credential_id_i64)
        .fetch_optional(&mut *tx)
        .await?
        .map(|row| runtime_state_from_row(&row))
        .transpose()?;
        tx.commit().await?;

        Ok(Some((credential, runtime_state)))
    }

    /// 查询未软删除的 API Key 凭据。
    ///
    /// 这里的“存在”只对应 `deleted_at IS NULL`，不等于已启用或可调度。
    /// 已禁用凭据仍然应该被识别为已有凭据，避免重启或导入时重复写入。
    pub async fn find_existing_api_key_credential(
        &self,
        api_key: &str,
    ) -> anyhow::Result<Option<KiroCredentials>> {
        let api_key = api_key.trim();
        if api_key.is_empty() {
            return Ok(None);
        }
        let row = sqlx::query(
            r#"
            SELECT id, priority, disabled, data, created_at, updated_at, revision
            FROM credentials
            WHERE deleted_at IS NULL
              AND api_key_hash = $1
            ORDER BY priority ASC, id ASC
            LIMIT 1
            "#,
        )
        .bind(sha256_hex(api_key))
        .fetch_optional(&self.pool)
        .await?;

        row.map(credential_from_row).transpose()
    }

    pub async fn ensure_api_key_credential(
        &self,
        api_key: &str,
    ) -> anyhow::Result<KiroCredentials> {
        let (api_key, region) =
            crate::kiro::model::credentials::split_kiro_api_key_and_region(api_key)
                .ok_or_else(|| anyhow::anyhow!("KIRO_API_KEY 为空"))?;
        if api_key.trim().is_empty() {
            anyhow::bail!("KIRO_API_KEY 为空");
        }
        if let Some(existing) = self.find_existing_api_key_credential(&api_key).await? {
            return Ok(existing);
        }

        let mut credential = KiroCredentials {
            kiro_api_key: Some(api_key.to_string()),
            auth_method: Some("api_key".to_string()),
            priority: 0,
            region: region.clone(),
            auth_region: region.clone(),
            api_region: region,
            ..Default::default()
        };
        credential.normalize_api_key_defaults();
        match self.insert_credential(&credential).await {
            Ok(inserted) => Ok(inserted),
            Err(err) if err.to_string().contains("kiroApiKey 重复") => self
                .find_existing_api_key_credential(&api_key)
                .await?
                .ok_or_else(|| anyhow::anyhow!("KIRO_API_KEY 已存在但重新查询失败")),
            Err(err) => Err(err),
        }
    }

    pub async fn credentials_exist(&self) -> anyhow::Result<bool> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM credentials WHERE deleted_at IS NULL")
                .fetch_one(&self.pool)
                .await?;
        Ok(count > 0)
    }

    pub async fn bootstrap_credentials_from_file(
        &self,
        credentials: &[KiroCredentials],
    ) -> anyhow::Result<()> {
        if self.credentials_exist().await? || credentials.is_empty() {
            return Ok(());
        }
        self.save_credentials(credentials).await?;
        Ok(())
    }

    async fn lock_credential_id_sequence_in_tx(
        tx: &mut Transaction<'_, Postgres>,
    ) -> anyhow::Result<()> {
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(CREDENTIAL_ID_SEQUENCE_LOCK_ID)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    async fn advance_credential_id_sequence_in_tx(
        tx: &mut Transaction<'_, Postgres>,
        credential_id: i64,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            SELECT setval(
                'credentials_id_seq',
                GREATEST(last_value, $1),
                is_called OR $1 >= last_value
            )
            FROM credentials_id_seq
            "#,
        )
        .bind(credential_id)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    async fn next_credential_id_in_tx(tx: &mut Transaction<'_, Postgres>) -> anyhow::Result<i64> {
        let max_id: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) FROM credentials")
            .fetch_one(&mut **tx)
            .await?;
        Self::advance_credential_id_sequence_in_tx(tx, max_id).await?;
        let id: i64 = sqlx::query_scalar("SELECT nextval('credentials_id_seq')")
            .fetch_one(&mut **tx)
            .await?;
        Ok(id)
    }

    /// 非破坏性保存凭据列表。
    ///
    /// 该方法用于首次 bootstrap 或补全旧凭据字段，只 upsert 传入行，不删除
    /// PgSQL 中其他未软删除凭据，避免旧进程内存快照覆盖其他实例新增的凭据。
    pub async fn save_credentials(
        &self,
        credentials: &[KiroCredentials],
    ) -> anyhow::Result<Vec<KiroCredentials>> {
        let mut saved = Vec::with_capacity(credentials.len());
        for credential in credentials {
            let authoritative = if credential.id.is_some() {
                match self.upsert_credential(credential).await? {
                    CredentialUpsertCasOutcome::Applied(saved) => saved,
                    CredentialUpsertCasOutcome::Conflict { current } => current,
                }
            } else {
                self.insert_credential(credential).await?
            };
            saved.push(authoritative);
        }
        Ok(saved)
    }

    pub async fn insert_credential(
        &self,
        credential: &KiroCredentials,
    ) -> anyhow::Result<KiroCredentials> {
        let mut canonical = credential.clone();
        canonical.storage_revision = 0;
        match self
            .upsert_credential_with_optional_id(&canonical, true)
            .await?
        {
            CredentialUpsertCasOutcome::Applied(credential) => Ok(credential),
            CredentialUpsertCasOutcome::Conflict { current } => {
                let id = current.id.unwrap_or_default();
                anyhow::bail!(
                    "凭据 #{} 已存在（当前 revision {}）",
                    id,
                    current.storage_revision
                )
            }
        }
    }

    pub async fn upsert_credential(
        &self,
        credential: &KiroCredentials,
    ) -> anyhow::Result<CredentialUpsertCasOutcome> {
        if credential.id.is_none() {
            anyhow::bail!("保存到 PgSQL 的凭据必须先分配 id");
        }
        self.upsert_credential_with_optional_id(credential, false)
            .await
    }

    async fn upsert_credential_with_optional_id(
        &self,
        credential: &KiroCredentials,
        allocate_missing_id: bool,
    ) -> anyhow::Result<CredentialUpsertCasOutcome> {
        if credential.id.is_none() && !allocate_missing_id {
            anyhow::bail!("保存到 PgSQL 的凭据必须先分配 id");
        }
        let expected_revision = i64::try_from(credential.storage_revision)
            .map_err(|_| anyhow::anyhow!("凭据 storage revision 超出 PgSQL BIGINT 范围"))?;
        let next_revision = credential
            .storage_revision
            .checked_add(1)
            .filter(|revision| i64::try_from(*revision).is_ok())
            .ok_or_else(|| anyhow::anyhow!("凭据 storage revision 已达到 PgSQL BIGINT 上限"))?;
        let mut tx = self.pool.begin().await?;
        if credential.storage_revision == 0 {
            Self::lock_credential_id_sequence_in_tx(&mut tx).await?;
        }
        let id_i64 = match credential.id {
            Some(id) => {
                i64::try_from(id).map_err(|_| anyhow::anyhow!("凭据 id 超出 PgSQL BIGINT 范围"))?
            }
            None if credential.storage_revision == 0 => {
                Self::next_credential_id_in_tx(&mut tx).await?
            }
            None => anyhow::bail!("保存到 PgSQL 的凭据必须先分配 id"),
        };
        let id = id_i64 as u64;
        let mut canonical = credential.clone();
        canonical.id = Some(id);
        canonical.canonicalize_auth_method();
        canonical.normalize_supported_models();
        canonical.normalize_api_key_defaults();
        canonical.normalize_external_idp_defaults();
        let priority = i32::try_from(canonical.priority)
            .map_err(|_| anyhow::anyhow!("凭据 priority 超出 PgSQL INTEGER 范围"))?;
        let (auth_kind, api_key_hash, refresh_token_hash) = credential_hash_columns(&canonical);
        let value = serde_json::to_value(&canonical)?;
        let applied = if canonical.storage_revision == 0 {
            sqlx::query(
                r#"
                INSERT INTO credentials (
                    id, priority, disabled, auth_kind, api_key_hash, refresh_token_hash,
                    data, updated_at, deleted_at, revision
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, now(), NULL, 1)
                ON CONFLICT (id) DO NOTHING
                RETURNING id, priority, disabled, data, created_at, updated_at, revision
                "#,
            )
            .bind(id_i64)
            .bind(priority)
            .bind(canonical.disabled)
            .bind(&auth_kind)
            .bind(&api_key_hash)
            .bind(&refresh_token_hash)
            .bind(&value)
            .fetch_optional(&mut *tx)
            .await
            .map_err(duplicate_credential_message)?
        } else {
            sqlx::query(
                r#"
                UPDATE credentials
                SET priority = $2,
                    disabled = $3,
                    auth_kind = $4,
                    api_key_hash = $5,
                    refresh_token_hash = $6,
                    data = $7,
                    updated_at = now(),
                    revision = credentials.revision + 1
                WHERE id = $1
                  AND deleted_at IS NULL
                  AND revision = $8
                RETURNING id, priority, disabled, data, created_at, updated_at, revision
                "#,
            )
            .bind(id_i64)
            .bind(priority)
            .bind(canonical.disabled)
            .bind(&auth_kind)
            .bind(&api_key_hash)
            .bind(&refresh_token_hash)
            .bind(&value)
            .bind(expected_revision)
            .fetch_optional(&mut *tx)
            .await
            .map_err(duplicate_credential_message)?
        };
        if let Some(row) = applied {
            let credential = credential_from_row(row)?;
            if credential.storage_revision != next_revision {
                anyhow::bail!(
                    "凭据 #{} storage revision 不一致：期望 {}，实际 {}",
                    id,
                    next_revision,
                    credential.storage_revision
                );
            }
            if canonical.storage_revision == 0 {
                Self::advance_credential_id_sequence_in_tx(&mut tx, id_i64).await?;
            }
            tx.commit().await?;
            return Ok(CredentialUpsertCasOutcome::Applied(credential));
        }

        let current = sqlx::query(
            r#"
            SELECT id, priority, disabled, data, created_at, updated_at, revision
            FROM credentials
            WHERE id = $1 AND deleted_at IS NULL
            FOR SHARE
            "#,
        )
        .bind(id_i64)
        .fetch_optional(&mut *tx)
        .await?
        .map(credential_from_row)
        .transpose()?;
        if let Some(current) = current {
            tx.commit().await?;
            return Ok(CredentialUpsertCasOutcome::Conflict { current });
        }

        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM credentials WHERE id = $1)")
                .bind(id_i64)
                .fetch_one(&mut *tx)
                .await?;
        tx.rollback().await?;
        if exists {
            anyhow::bail!("凭据 #{} 已被删除，禁止通过 upsert 恢复", id);
        }
        anyhow::bail!("凭据 #{} 不存在，无法按 revision 更新", id)
    }

    pub async fn insert_credential_with_runtime_patch(
        &self,
        credential: &KiroCredentials,
        operation_id: Uuid,
        patch: &CredentialRuntimeStatePatch,
    ) -> anyhow::Result<(KiroCredentials, CredentialRuntimeStateMutationResult)> {
        validate_credential_runtime_state_patch(patch)?;
        let mut tx = self.pool.begin().await?;
        Self::lock_credential_id_sequence_in_tx(&mut tx).await?;
        let id = match credential.id {
            Some(id) => id,
            None => {
                let id = Self::next_credential_id_in_tx(&mut tx).await?;
                id.max(0) as u64
            }
        };
        let id_i64 =
            i64::try_from(id).map_err(|_| anyhow::anyhow!("凭据 id 超出 PgSQL BIGINT 范围"))?;
        let mut canonical = credential.clone();
        canonical.id = Some(id);
        canonical.storage_revision = 0;
        if let Some(disabled) = patch.credential_disabled {
            canonical.disabled = disabled;
        }
        canonical.canonicalize_auth_method();
        canonical.normalize_supported_models();
        canonical.normalize_api_key_defaults();
        canonical.normalize_external_idp_defaults();
        let priority = i32::try_from(canonical.priority)
            .map_err(|_| anyhow::anyhow!("凭据 priority 超出 PgSQL INTEGER 范围"))?;
        let (auth_kind, api_key_hash, refresh_token_hash) = credential_hash_columns(&canonical);
        let value = serde_json::to_value(&canonical)?;
        let row = sqlx::query(
            r#"
            INSERT INTO credentials (
                id, priority, disabled, auth_kind, api_key_hash, refresh_token_hash,
                data, updated_at, deleted_at, revision
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, now(), NULL, 1)
            RETURNING id, priority, disabled, data, created_at, updated_at, revision
            "#,
        )
        .bind(id_i64)
        .bind(priority)
        .bind(canonical.disabled)
        .bind(auth_kind)
        .bind(api_key_hash)
        .bind(refresh_token_hash)
        .bind(value)
        .fetch_one(&mut *tx)
        .await
        .map_err(duplicate_credential_message)?;
        let inserted = credential_from_row(row)?;
        if inserted.storage_revision != 1 {
            anyhow::bail!("新凭据 #{} 的 storage revision 必须为 1", id);
        }
        Self::advance_credential_id_sequence_in_tx(&mut tx, id_i64).await?;

        let mut runtime_patch = patch.clone();
        runtime_patch.credential_disabled = None;
        let runtime = Self::patch_credential_runtime_state_in_tx(
            &mut tx,
            id,
            operation_id,
            "initial_patch",
            &runtime_patch,
        )
        .await?;
        if !runtime.applied {
            tx.rollback().await?;
            anyhow::bail!(
                "新凭据 #{} 的初始运行态 generation 已过期：当前 {}",
                id,
                runtime.state.generation
            );
        }
        tx.commit().await?;
        Ok((inserted, runtime))
    }

    pub async fn update_credential_with_runtime_patch_cas(
        &self,
        credential: &KiroCredentials,
        operation_id: Uuid,
        patch: &CredentialRuntimeStatePatch,
    ) -> anyhow::Result<CredentialWithRuntimePatchCasOutcome> {
        validate_credential_runtime_state_patch(patch)?;
        let id = credential
            .id
            .ok_or_else(|| anyhow::anyhow!("更新 PgSQL 凭据必须提供 id"))?;
        if credential.storage_revision == 0 {
            anyhow::bail!(
                "凭据 #{} runtime patch 更新必须提供非零 storage revision",
                id
            );
        }
        let id_i64 =
            i64::try_from(id).map_err(|_| anyhow::anyhow!("凭据 id 超出 PgSQL BIGINT 范围"))?;
        let expected_revision = i64::try_from(credential.storage_revision)
            .map_err(|_| anyhow::anyhow!("凭据 storage revision 超出 PgSQL BIGINT 范围"))?;
        let next_revision = credential
            .storage_revision
            .checked_add(1)
            .filter(|revision| i64::try_from(*revision).is_ok())
            .ok_or_else(|| anyhow::anyhow!("凭据 storage revision 已达到 PgSQL BIGINT 上限"))?;
        let mut canonical = credential.clone();
        if let Some(disabled) = patch.credential_disabled {
            canonical.disabled = disabled;
        }
        canonical.canonicalize_auth_method();
        canonical.normalize_supported_models();
        canonical.normalize_api_key_defaults();
        canonical.normalize_external_idp_defaults();
        let priority = i32::try_from(canonical.priority)
            .map_err(|_| anyhow::anyhow!("凭据 priority 超出 PgSQL INTEGER 范围"))?;
        let (auth_kind, api_key_hash, refresh_token_hash) = credential_hash_columns(&canonical);
        let value = serde_json::to_value(&canonical)?;

        let mut tx = self.pool.begin().await?;
        let applied = sqlx::query(
            r#"
            UPDATE credentials
            SET priority = $2,
                disabled = $3,
                auth_kind = $4,
                api_key_hash = $5,
                refresh_token_hash = $6,
                data = $7,
                updated_at = now(),
                revision = credentials.revision + 1
            WHERE id = $1
              AND deleted_at IS NULL
              AND revision = $8
            RETURNING id, priority, disabled, data, created_at, updated_at, revision
            "#,
        )
        .bind(id_i64)
        .bind(priority)
        .bind(canonical.disabled)
        .bind(auth_kind)
        .bind(api_key_hash)
        .bind(refresh_token_hash)
        .bind(value)
        .bind(expected_revision)
        .fetch_optional(&mut *tx)
        .await
        .map_err(duplicate_credential_message)?;
        let Some(row) = applied else {
            let current = sqlx::query(
                r#"
                SELECT id, priority, disabled, data, created_at, updated_at, revision
                FROM credentials
                WHERE id = $1 AND deleted_at IS NULL
                FOR SHARE
                "#,
            )
            .bind(id_i64)
            .fetch_optional(&mut *tx)
            .await?
            .map(credential_from_row)
            .transpose()?;
            if let Some(current) = current {
                tx.commit().await?;
                return Ok(CredentialWithRuntimePatchCasOutcome::Conflict { current });
            }
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM credentials WHERE id = $1)")
                    .bind(id_i64)
                    .fetch_one(&mut *tx)
                    .await?;
            tx.rollback().await?;
            if exists {
                anyhow::bail!("凭据 #{} 已被删除，禁止更新", id);
            }
            anyhow::bail!("凭据 #{} 不存在，无法更新", id);
        };
        let updated = credential_from_row(row)?;
        if updated.storage_revision != next_revision {
            anyhow::bail!(
                "凭据 #{} storage revision 不一致：期望 {}，实际 {}",
                id,
                next_revision,
                updated.storage_revision
            );
        }

        let mut runtime_patch = patch.clone();
        runtime_patch.credential_disabled = None;
        let runtime = Self::patch_credential_runtime_state_in_tx(
            &mut tx,
            id,
            operation_id,
            "credential_update_patch",
            &runtime_patch,
        )
        .await?;
        if !runtime.applied {
            tx.rollback().await?;
            anyhow::bail!(
                "凭据 #{} 的原子运行态 generation 已过期：当前 {}",
                id,
                runtime.state.generation
            );
        }
        tx.commit().await?;
        Ok(CredentialWithRuntimePatchCasOutcome::Applied {
            credential: updated,
            runtime,
        })
    }

    pub async fn update_credential_refresh_fields_cas(
        &self,
        credential_id: u64,
        expected: &CredentialRefreshExpectedContext,
        patch: &CredentialRefreshFieldsPatch,
    ) -> anyhow::Result<CredentialRefreshFieldsCasOutcome> {
        if patch
            .refresh_token
            .as_deref()
            .is_some_and(|token| token.trim().is_empty())
        {
            anyhow::bail!("刷新后的 refreshToken 不能为空");
        }
        if patch.access_token.is_none()
            && patch.refresh_token.is_none()
            && patch.profile_arn.is_none()
            && patch.expires_at.is_none()
            && patch.scopes.is_none()
        {
            anyhow::bail!("凭据 refresh 字段更新不能为空");
        }

        let new_refresh_token_hash = patch.refresh_token.as_deref().map(sha256_hex);
        let mut tx = self.pool.begin().await?;
        let applied = sqlx::query(
            r#"
            UPDATE credentials
            SET refresh_token_hash = CASE
                    WHEN $11::text IS NULL THEN credentials.refresh_token_hash
                    ELSE $12::text
                END,
                data = credentials.data
                    || CASE WHEN $10::text IS NULL
                        THEN '{}'::jsonb
                        ELSE jsonb_build_object('accessToken', $10::text)
                    END
                    || CASE WHEN $11::text IS NULL
                        THEN '{}'::jsonb
                        ELSE jsonb_build_object('refreshToken', $11::text)
                    END
                    || CASE WHEN $13::text IS NULL
                        THEN '{}'::jsonb
                        ELSE jsonb_build_object('profileArn', $13::text)
                    END
                    || CASE WHEN $14::text IS NULL
                        THEN '{}'::jsonb
                        ELSE jsonb_build_object('expiresAt', $14::text)
                    END
                    || CASE WHEN $15::text IS NULL
                        THEN '{}'::jsonb
                        ELSE jsonb_build_object('scopes', $15::text)
                    END,
                updated_at = now(),
                revision = credentials.revision + 1
            WHERE id = $1
              AND deleted_at IS NULL
              AND refresh_token_hash = $2
              AND (
                  COALESCE(data->>'accessToken', data->>'access_token')
                  IS NOT DISTINCT FROM $9::text
              )
              AND (
                  CASE regexp_replace(
                      lower(COALESCE(data->>'authMethod', data->>'auth_method')),
                      '[^a-z0-9]',
                      '',
                      'g'
                  )
                      WHEN 'builderid' THEN 'idc'
                      WHEN 'iam' THEN 'idc'
                      WHEN 'idc' THEN 'idc'
                      WHEN 'apikey' THEN 'api_key'
                      WHEN 'externalidp' THEN 'external_idp'
                      WHEN 'enterprise' THEN 'external_idp'
                      WHEN 'iamsso' THEN 'external_idp'
                      WHEN 'awsidc' THEN 'external_idp'
                      WHEN 'internal' THEN 'external_idp'
                      WHEN 'social' THEN 'social'
                      ELSE COALESCE(data->>'authMethod', data->>'auth_method')
                  END
                  IS NOT DISTINCT FROM $3::text
              )
              AND (data->>'provider' IS NOT DISTINCT FROM $4::text)
              AND (
                  COALESCE(data->>'clientId', data->>'client_id')
                  IS NOT DISTINCT FROM $5::text
              )
              AND (
                  COALESCE(data->>'clientSecret', data->>'client_secret')
                  IS NOT DISTINCT FROM $6::text
              )
              AND (
                  COALESCE(data->>'tokenEndpoint', data->>'token_endpoint')
                  IS NOT DISTINCT FROM $7::text
              )
              AND (
                  COALESCE(data->>'scopes', data->>'scope')
                  IS NOT DISTINCT FROM $8::text
              )
            RETURNING id, priority, disabled, data, created_at, updated_at, revision
            "#,
        )
        .bind(credential_id as i64)
        .bind(&expected.refresh_token_hash)
        .bind(&expected.auth_method)
        .bind(&expected.provider)
        .bind(&expected.client_id)
        .bind(&expected.client_secret)
        .bind(&expected.token_endpoint)
        .bind(&expected.scopes)
        .bind(&expected.access_token)
        .bind(&patch.access_token)
        .bind(&patch.refresh_token)
        .bind(&new_refresh_token_hash)
        .bind(&patch.profile_arn)
        .bind(&patch.expires_at)
        .bind(&patch.scopes)
        .fetch_optional(&mut *tx)
        .await
        .map_err(duplicate_credential_message)?;
        if let Some(row) = applied {
            let credential = credential_from_row(row)?;
            tx.commit().await?;
            return Ok(CredentialRefreshFieldsCasOutcome::Applied(credential));
        }

        let current = sqlx::query(
            r#"
            SELECT id, priority, disabled, data, created_at, updated_at, revision
            FROM credentials
            WHERE id = $1 AND deleted_at IS NULL
            FOR SHARE
            "#,
        )
        .bind(credential_id as i64)
        .fetch_optional(&mut *tx)
        .await?
        .map(credential_from_row)
        .transpose()?;
        tx.commit().await?;
        Ok(CredentialRefreshFieldsCasOutcome::Conflict { current })
    }

    pub async fn soft_delete_credential(&self, credential_id: u64) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            UPDATE credentials
            SET deleted_at = now(),
                updated_at = now(),
                revision = credentials.revision + 1
            WHERE id = $1 AND deleted_at IS NULL
            "#,
        )
        .bind(credential_id as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn load_proxy_resources(&self) -> anyhow::Result<Vec<ProxyResourceRow>> {
        let rows = sqlx::query(
            r#"
            SELECT pr.id, pr.name, pr.proxy_url, pr.proxy_username, pr.proxy_password,
                   pr.enabled, pr.notes, pr.created_at, pr.updated_at,
                   COUNT(c.id) FILTER (WHERE c.deleted_at IS NULL) AS credential_count
            FROM proxy_resources pr
            LEFT JOIN credentials c
              ON c.deleted_at IS NULL
             AND (c.data->>'proxyResourceId') ~ '^[0-9]+$'
             AND (c.data->>'proxyResourceId')::BIGINT = pr.id
            WHERE pr.deleted_at IS NULL
            GROUP BY pr.id
            ORDER BY pr.id ASC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(proxy_resource_from_row).collect()
    }

    pub async fn get_proxy_resource(&self, id: u64) -> anyhow::Result<Option<ProxyResourceRow>> {
        let row = sqlx::query(
            r#"
            SELECT pr.id, pr.name, pr.proxy_url, pr.proxy_username, pr.proxy_password,
                   pr.enabled, pr.notes, pr.created_at, pr.updated_at,
                   COUNT(c.id) FILTER (WHERE c.deleted_at IS NULL) AS credential_count
            FROM proxy_resources pr
            LEFT JOIN credentials c
              ON c.deleted_at IS NULL
             AND (c.data->>'proxyResourceId') ~ '^[0-9]+$'
             AND (c.data->>'proxyResourceId')::BIGINT = pr.id
            WHERE pr.id = $1 AND pr.deleted_at IS NULL
            GROUP BY pr.id
            "#,
        )
        .bind(id as i64)
        .fetch_optional(&self.pool)
        .await?;

        row.map(proxy_resource_from_row).transpose()
    }

    pub async fn soft_delete_proxy_resource_if_unbound(
        &self,
        id: u64,
    ) -> anyhow::Result<Option<u64>> {
        let mut tx = self.pool.begin().await?;
        let exists = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT id
            FROM proxy_resources
            WHERE id = $1 AND deleted_at IS NULL
            FOR UPDATE
            "#,
        )
        .bind(id as i64)
        .fetch_optional(&mut *tx)
        .await?;

        if exists.is_none() {
            tx.rollback().await?;
            return Ok(None);
        }

        let credential_count = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT COUNT(*)
            FROM credentials
            WHERE deleted_at IS NULL
              AND (data->>'proxyResourceId') ~ '^[0-9]+$'
              AND (data->>'proxyResourceId')::BIGINT = $1
            "#,
        )
        .bind(id as i64)
        .fetch_one(&mut *tx)
        .await?;
        let credential_count = credential_count.max(0) as u64;

        if credential_count > 0 {
            tx.rollback().await?;
            return Ok(Some(credential_count));
        }

        sqlx::query(
            r#"
            UPDATE proxy_resources
            SET deleted_at = now(),
                enabled = false,
                updated_at = now()
            WHERE id = $1 AND deleted_at IS NULL
            "#,
        )
        .bind(id as i64)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(Some(0))
    }

    pub async fn insert_proxy_resource(
        &self,
        resource: &CreateProxyResourceRow,
    ) -> anyhow::Result<ProxyResourceRow> {
        let row = sqlx::query(
            r#"
            INSERT INTO proxy_resources (
                name, proxy_url, proxy_username, proxy_password,
                enabled, notes, updated_at, deleted_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, now(), NULL)
            RETURNING id, name, proxy_url, proxy_username, proxy_password,
                      enabled, notes, created_at, updated_at,
                      0::BIGINT AS credential_count
            "#,
        )
        .bind(&resource.name)
        .bind(&resource.proxy_url)
        .bind(&resource.proxy_username)
        .bind(&resource.proxy_password)
        .bind(resource.enabled)
        .bind(&resource.notes)
        .fetch_one(&self.pool)
        .await?;
        proxy_resource_from_row(row)
    }

    pub async fn update_proxy_resource(
        &self,
        id: u64,
        update: &UpdateProxyResourceRow,
    ) -> anyhow::Result<Option<ProxyResourceRow>> {
        sqlx::query(
            r#"
            UPDATE proxy_resources
            SET name = COALESCE($2, name),
                proxy_url = COALESCE($3, proxy_url),
                proxy_username = CASE WHEN $4 THEN $5 ELSE proxy_username END,
                proxy_password = CASE WHEN $6 THEN $7 ELSE proxy_password END,
                enabled = COALESCE($8, enabled),
                notes = CASE WHEN $9 THEN $10 ELSE notes END,
                updated_at = now()
            WHERE id = $1 AND deleted_at IS NULL
            "#,
        )
        .bind(id as i64)
        .bind(&update.name)
        .bind(&update.proxy_url)
        .bind(update.proxy_username.is_some())
        .bind(update.proxy_username.clone().flatten())
        .bind(update.proxy_password.is_some())
        .bind(update.proxy_password.clone().flatten())
        .bind(update.enabled)
        .bind(update.notes.is_some())
        .bind(update.notes.clone().flatten())
        .execute(&self.pool)
        .await?;

        self.get_proxy_resource(id).await
    }

    pub async fn delete_credential_stats_and_runtime(
        &self,
        credential_id: u64,
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM credential_runtime_state WHERE credential_id = $1")
            .bind(credential_id as i64)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM credential_stats WHERE credential_id = $1")
            .bind(credential_id as i64)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn load_credential_stats(&self) -> anyhow::Result<HashMap<u64, CredentialStatsRow>> {
        let rows = sqlx::query(
            "SELECT credential_id, success_count, selection_count, last_used_at FROM credential_stats",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut stats = HashMap::with_capacity(rows.len());
        for row in rows {
            let credential_id: i64 = row.try_get("credential_id")?;
            let success_count: i64 = row.try_get("success_count")?;
            let selection_count: i64 = row.try_get("selection_count")?;
            let last_used_at: Option<String> = row.try_get("last_used_at")?;
            stats.insert(
                credential_id as u64,
                CredentialStatsRow {
                    success_count: success_count.max(0) as u64,
                    selection_count: selection_count.max(0) as u64,
                    last_used_at,
                },
            );
        }
        Ok(stats)
    }

    pub async fn load_credential_runtime_state(
        &self,
    ) -> anyhow::Result<HashMap<u64, CredentialRuntimeStateRow>> {
        let rows = sqlx::query(CREDENTIAL_RUNTIME_STATE_SELECT_SQL)
            .fetch_all(&self.pool)
            .await?;
        credential_runtime_states_from_rows(rows)
    }

    pub async fn cleanup_credential_runtime_mutations(
        &self,
        retention: std::time::Duration,
        limit: usize,
    ) -> anyhow::Result<u64> {
        const MAX_CLEANUP_BATCH: usize = 10_000;
        let limit = limit.min(MAX_CLEANUP_BATCH);
        if limit == 0 {
            return Ok(0);
        }
        let retention_micros = i64::try_from(retention.as_micros())
            .map_err(|_| anyhow::anyhow!("credential mutation retention 超出 PgSQL 范围"))?;
        let removed: i64 = sqlx::query_scalar(
            r#"
            WITH runtime_candidates AS (
                SELECT operation_id, created_at
                FROM credential_runtime_mutations
                WHERE created_at < now() - ($1::bigint * interval '1 microsecond')
                ORDER BY created_at ASC, operation_id ASC
                LIMIT $2
                FOR UPDATE SKIP LOCKED
            ),
            stats_candidates AS (
                SELECT operation_id, created_at
                FROM credential_stats_delta_batches
                WHERE created_at < now() - ($1::bigint * interval '1 microsecond')
                ORDER BY created_at ASC, operation_id ASC
                LIMIT $2
                FOR UPDATE SKIP LOCKED
            ),
            expired AS (
                SELECT ledger_kind, operation_id
                FROM (
                    SELECT 'runtime'::text AS ledger_kind,
                           operation_id, created_at
                    FROM runtime_candidates
                    UNION ALL
                    SELECT 'stats'::text AS ledger_kind,
                           operation_id, created_at
                    FROM stats_candidates
                ) AS candidates
                ORDER BY created_at ASC, ledger_kind ASC, operation_id ASC
                LIMIT $2
            ),
            deleted_runtime AS (
                DELETE FROM credential_runtime_mutations AS mutation
                USING expired
                WHERE expired.ledger_kind = 'runtime'
                  AND mutation.operation_id = expired.operation_id
                RETURNING 1
            ),
            deleted_stats AS (
                DELETE FROM credential_stats_delta_batches AS batch
                USING expired
                WHERE expired.ledger_kind = 'stats'
                  AND batch.operation_id = expired.operation_id
                RETURNING 1
            )
            SELECT (SELECT COUNT(*) FROM deleted_runtime)
                 + (SELECT COUNT(*) FROM deleted_stats)
            "#,
        )
        .bind(retention_micros)
        .bind(limit as i64)
        .fetch_one(&self.pool)
        .await?;
        Ok(removed.max(0) as u64)
    }

    pub async fn load_credential_account_info(
        &self,
    ) -> anyhow::Result<HashMap<u64, CredentialAccountInfoRow>> {
        let rows = sqlx::query(
            r#"
            SELECT credential_id, subscription_title, current_usage, usage_limit,
                   remaining, usage_percentage, credit_limit, credit_remaining,
                   credit_base, credit_bonus, overage_status, overage_capability,
                   overage_cap, overage_rate, current_overages, next_reset_at, checked_at
            FROM credential_account_info
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        let mut info = HashMap::with_capacity(rows.len());
        for row in rows {
            let credential_id: i64 = row.try_get("credential_id")?;
            let checked_at: DateTime<Utc> = row.try_get("checked_at")?;
            info.insert(
                credential_id as u64,
                CredentialAccountInfoRow {
                    subscription_title: row.try_get("subscription_title")?,
                    current_usage: row.try_get("current_usage")?,
                    usage_limit: row.try_get("usage_limit")?,
                    remaining: row.try_get("remaining")?,
                    usage_percentage: row.try_get("usage_percentage")?,
                    credit_limit: row.try_get("credit_limit")?,
                    credit_remaining: row.try_get("credit_remaining")?,
                    credit_base: row.try_get("credit_base")?,
                    credit_bonus: row.try_get("credit_bonus")?,
                    overage_status: row.try_get("overage_status")?,
                    overage_capability: row.try_get("overage_capability")?,
                    overage_cap: row.try_get("overage_cap")?,
                    overage_rate: row.try_get("overage_rate")?,
                    current_overages: row.try_get("current_overages")?,
                    next_reset_at: row.try_get("next_reset_at")?,
                    checked_at: checked_at.to_rfc3339(),
                },
            );
        }
        Ok(info)
    }

    pub async fn load_credential_account_info_for_ids(
        &self,
        ids: &[u64],
    ) -> anyhow::Result<HashMap<u64, CredentialAccountInfoRow>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let credential_ids: Vec<i64> = ids.iter().map(|id| *id as i64).collect();
        let rows = sqlx::query(
            r#"
            SELECT credential_id, subscription_title, current_usage, usage_limit,
                   remaining, usage_percentage, credit_limit, credit_remaining,
                   credit_base, credit_bonus, overage_status, overage_capability,
                   overage_cap, overage_rate, current_overages, next_reset_at, checked_at
            FROM credential_account_info
            WHERE credential_id = ANY($1)
            "#,
        )
        .bind(&credential_ids)
        .fetch_all(&self.pool)
        .await?;

        let mut info = HashMap::with_capacity(rows.len());
        for row in rows {
            let credential_id: i64 = row.try_get("credential_id")?;
            let checked_at: DateTime<Utc> = row.try_get("checked_at")?;
            info.insert(
                credential_id as u64,
                CredentialAccountInfoRow {
                    subscription_title: row.try_get("subscription_title")?,
                    current_usage: row.try_get("current_usage")?,
                    usage_limit: row.try_get("usage_limit")?,
                    remaining: row.try_get("remaining")?,
                    usage_percentage: row.try_get("usage_percentage")?,
                    credit_limit: row.try_get("credit_limit")?,
                    credit_remaining: row.try_get("credit_remaining")?,
                    credit_base: row.try_get("credit_base")?,
                    credit_bonus: row.try_get("credit_bonus")?,
                    overage_status: row.try_get("overage_status")?,
                    overage_capability: row.try_get("overage_capability")?,
                    overage_cap: row.try_get("overage_cap")?,
                    overage_rate: row.try_get("overage_rate")?,
                    current_overages: row.try_get("current_overages")?,
                    next_reset_at: row.try_get("next_reset_at")?,
                    checked_at: checked_at.to_rfc3339(),
                },
            );
        }
        Ok(info)
    }

    pub async fn save_credential_account_info(
        &self,
        credential_id: u64,
        info: &CredentialAccountInfoRow,
    ) -> anyhow::Result<()> {
        let checked_at = DateTime::parse_from_rfc3339(&info.checked_at)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
        sqlx::query(
            r#"
            INSERT INTO credential_account_info (
                credential_id, subscription_title, current_usage, usage_limit,
                remaining, usage_percentage, credit_limit, credit_remaining,
                credit_base, credit_bonus, overage_status, overage_capability,
                overage_cap, overage_rate, current_overages, next_reset_at, checked_at, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, now())
            ON CONFLICT (credential_id) DO UPDATE
            SET subscription_title = EXCLUDED.subscription_title,
                current_usage = EXCLUDED.current_usage,
                usage_limit = EXCLUDED.usage_limit,
                remaining = EXCLUDED.remaining,
                usage_percentage = EXCLUDED.usage_percentage,
                credit_limit = EXCLUDED.credit_limit,
                credit_remaining = EXCLUDED.credit_remaining,
                credit_base = EXCLUDED.credit_base,
                credit_bonus = EXCLUDED.credit_bonus,
                overage_status = EXCLUDED.overage_status,
                overage_capability = EXCLUDED.overage_capability,
                overage_cap = EXCLUDED.overage_cap,
                overage_rate = EXCLUDED.overage_rate,
                current_overages = EXCLUDED.current_overages,
                next_reset_at = EXCLUDED.next_reset_at,
                checked_at = EXCLUDED.checked_at,
                updated_at = now()
            "#,
        )
        .bind(credential_id as i64)
        .bind(&info.subscription_title)
        .bind(info.current_usage)
        .bind(info.usage_limit)
        .bind(info.remaining)
        .bind(info.usage_percentage)
        .bind(info.credit_limit)
        .bind(info.credit_remaining)
        .bind(info.credit_base)
        .bind(info.credit_bonus)
        .bind(&info.overage_status)
        .bind(&info.overage_capability)
        .bind(info.overage_cap)
        .bind(info.overage_rate)
        .bind(info.current_overages)
        .bind(info.next_reset_at)
        .bind(checked_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    #[allow(dead_code)]
    pub async fn save_credential_runtime_state(
        &self,
        states: &HashMap<u64, CredentialRuntimeStateRow>,
    ) -> anyhow::Result<HashMap<u64, CredentialRuntimeStateCasOutcome>> {
        let snapshots = states
            .iter()
            .map(|(credential_id, state)| {
                (
                    *credential_id,
                    CredentialRuntimeStateSnapshot {
                        state: state.clone(),
                        expected_revision: state.revision,
                    },
                )
            })
            .collect();
        self.apply_credential_runtime_state_snapshots(&snapshots)
            .await
    }

    #[allow(dead_code)]
    pub async fn save_credential_runtime_state_for(
        &self,
        credential_id: u64,
        state: &CredentialRuntimeStateRow,
    ) -> anyhow::Result<CredentialRuntimeStateCasOutcome> {
        let snapshot = CredentialRuntimeStateSnapshot {
            state: state.clone(),
            expected_revision: state.revision,
        };
        self.save_credential_runtime_state_snapshot(credential_id, &snapshot)
            .await
    }

    pub async fn save_credential_runtime_state_snapshot(
        &self,
        credential_id: u64,
        snapshot: &CredentialRuntimeStateSnapshot,
    ) -> anyhow::Result<CredentialRuntimeStateCasOutcome> {
        if snapshot.state.revision != snapshot.expected_revision {
            anyhow::bail!(
                "凭据 #{} snapshot revision {} 与 expected revision {} 不一致",
                credential_id,
                snapshot.state.revision,
                snapshot.expected_revision
            );
        }
        let expected_revision = i64::try_from(snapshot.expected_revision)
            .map_err(|_| anyhow::anyhow!("凭据运行态 expected revision 超出 PgSQL BIGINT 范围"))?;
        let expected_generation = i64::try_from(snapshot.state.generation)
            .map_err(|_| anyhow::anyhow!("凭据运行态 generation 超出 PgSQL BIGINT 范围"))?;
        let next_revision = snapshot
            .expected_revision
            .checked_add(1)
            .filter(|revision| i64::try_from(*revision).is_ok())
            .ok_or_else(|| anyhow::anyhow!("凭据运行态 revision 已达到 PgSQL BIGINT 上限"))?;
        let mut tx = self.pool.begin().await?;
        lock_active_credential_in_tx(&mut tx, credential_id).await?;
        let applied = if snapshot.expected_revision == 0 {
            sqlx::query(
                r#"
                INSERT INTO credential_runtime_state (
                    credential_id, failure_count, refresh_failure_count,
                    disabled_reason, warmup_remaining, generation, revision, updated_at
                )
                VALUES ($1, $2, $3, $4, $5, $6, 1, now())
                ON CONFLICT (credential_id) DO NOTHING
                RETURNING failure_count, refresh_failure_count, disabled_reason,
                          warmup_remaining, generation, revision
                "#,
            )
            .bind(credential_id as i64)
            .bind(snapshot.state.failure_count as i32)
            .bind(snapshot.state.refresh_failure_count as i32)
            .bind(&snapshot.state.disabled_reason)
            .bind(snapshot.state.warmup_remaining as i32)
            .bind(expected_generation)
            .fetch_optional(&mut *tx)
            .await?
        } else {
            sqlx::query(
                r#"
                UPDATE credential_runtime_state
                SET failure_count = $2,
                    refresh_failure_count = $3,
                    disabled_reason = $4,
                    warmup_remaining = $5,
                    revision = credential_runtime_state.revision + 1,
                    updated_at = now()
                WHERE credential_id = $1
                  AND credential_runtime_state.generation = $6
                  AND credential_runtime_state.revision = $7
                RETURNING failure_count, refresh_failure_count, disabled_reason,
                          warmup_remaining, generation, revision
                "#,
            )
            .bind(credential_id as i64)
            .bind(snapshot.state.failure_count as i32)
            .bind(snapshot.state.refresh_failure_count as i32)
            .bind(&snapshot.state.disabled_reason)
            .bind(snapshot.state.warmup_remaining as i32)
            .bind(expected_generation)
            .bind(expected_revision)
            .fetch_optional(&mut *tx)
            .await?
        };
        if let Some(row) = applied {
            let state = runtime_state_from_row(&row)?;
            verify_runtime_mutation_revision(&state, next_revision)?;
            verify_runtime_mutation_generation(&state, snapshot.state.generation)?;
            tx.commit().await?;
            return Ok(CredentialRuntimeStateCasOutcome::Applied(state));
        }

        let current = sqlx::query(
            r#"
            SELECT failure_count, refresh_failure_count, disabled_reason,
                   warmup_remaining, generation, revision
            FROM credential_runtime_state
            WHERE credential_id = $1
            FOR SHARE
            "#,
        )
        .bind(credential_id as i64)
        .fetch_optional(&mut *tx)
        .await?
        .map(|row| runtime_state_from_row(&row))
        .transpose()?;
        tx.commit().await?;
        Ok(CredentialRuntimeStateCasOutcome::Conflict { current })
    }

    #[cfg(test)]
    pub async fn save_credential_stats(
        &self,
        stats: &HashMap<u64, CredentialStatsRow>,
    ) -> anyhow::Result<()> {
        for (credential_id, stat) in stats {
            self.save_credential_stats_for(*credential_id, stat).await?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub async fn save_credential_stats_for(
        &self,
        credential_id: u64,
        stat: &CredentialStatsRow,
    ) -> anyhow::Result<()> {
        if let Some(last_used_at) = stat.last_used_at.as_deref() {
            validate_rfc3339_timestamp("last_used_at", last_used_at)?;
        }
        sqlx::query(
            r#"
            INSERT INTO credential_stats (
                credential_id, success_count, selection_count, last_used_at, updated_at
            )
            VALUES ($1, $2, $3, $4, now())
            ON CONFLICT (credential_id) DO UPDATE
            SET success_count = GREATEST(credential_stats.success_count, EXCLUDED.success_count),
                selection_count = GREATEST(credential_stats.selection_count, EXCLUDED.selection_count),
                last_used_at = CASE
                    WHEN EXCLUDED.last_used_at IS NULL THEN credential_stats.last_used_at
                    WHEN credential_stats.last_used_at IS NULL THEN EXCLUDED.last_used_at
                    WHEN EXCLUDED.last_used_at::timestamptz
                         >= credential_stats.last_used_at::timestamptz
                        THEN EXCLUDED.last_used_at
                    ELSE credential_stats.last_used_at
                END,
                updated_at = now()
            "#,
        )
        .bind(credential_id as i64)
        .bind(stat.success_count as i64)
        .bind(stat.selection_count as i64)
        .bind(&stat.last_used_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn apply_credential_stats_deltas(
        &self,
        operation_id: Uuid,
        deltas: &HashMap<u64, CredentialStatsDeltaRow>,
    ) -> anyhow::Result<()> {
        let mut rows = Vec::with_capacity(deltas.len());
        for (credential_id, delta) in deltas {
            if delta.success_delta == 0
                && delta.selection_delta == 0
                && delta.last_used_at.is_none()
            {
                continue;
            }
            if let Some(last_used_at) = delta.last_used_at.as_deref() {
                validate_rfc3339_timestamp("last_used_at", last_used_at)?;
            }
            rows.push((
                i64::try_from(*credential_id).map_err(|_| {
                    anyhow::anyhow!("凭据统计增量 credential_id 超出 PgSQL BIGINT 范围")
                })?,
                i64::try_from(delta.success_delta).map_err(|_| {
                    anyhow::anyhow!("凭据统计 success_delta 超出 PgSQL BIGINT 范围")
                })?,
                i64::try_from(delta.selection_delta).map_err(|_| {
                    anyhow::anyhow!("凭据统计 selection_delta 超出 PgSQL BIGINT 范围")
                })?,
                delta.last_used_at.clone(),
            ));
        }
        rows.sort_unstable_by_key(|row| row.0);
        let payload_hash = sha256_hex(&serde_json::to_string(&rows)?);
        let input_credential_count = i32::try_from(rows.len())
            .map_err(|_| anyhow::anyhow!("凭据统计增量批次行数超出 PgSQL INTEGER 范围"))?;

        let mut tx = self.pool.begin().await?;
        let credential_ids = rows.iter().map(|row| row.0).collect::<Vec<_>>();
        let active_credential_ids: Vec<i64> = if credential_ids.is_empty() {
            Vec::new()
        } else {
            sqlx::query_scalar(
                r#"
                SELECT id
                FROM credentials
                WHERE deleted_at IS NULL
                  AND id = ANY($1)
                ORDER BY id ASC
                FOR SHARE
                "#,
            )
            .bind(&credential_ids)
            .fetch_all(&mut *tx)
            .await?
        };
        rows.retain(|row| active_credential_ids.binary_search(&row.0).is_ok());
        let applied_credential_count = i32::try_from(rows.len())
            .map_err(|_| anyhow::anyhow!("凭据统计有效增量行数超出 PgSQL INTEGER 范围"))?;

        let operation_id = operation_id.to_string();
        let inserted: Option<String> = sqlx::query_scalar(
            r#"
            INSERT INTO credential_stats_delta_batches (
                operation_id, payload_hash, input_credential_count,
                applied_credential_count, created_at
            )
            VALUES ($1, $2, $3, $4, now())
            ON CONFLICT (operation_id) DO NOTHING
            RETURNING operation_id
            "#,
        )
        .bind(&operation_id)
        .bind(&payload_hash)
        .bind(input_credential_count)
        .bind(applied_credential_count)
        .fetch_optional(&mut *tx)
        .await?;
        if inserted.is_none() {
            let existing_payload_hash: String = sqlx::query_scalar(
                r#"
                SELECT payload_hash
                FROM credential_stats_delta_batches
                WHERE operation_id = $1
                FOR UPDATE
                "#,
            )
            .bind(&operation_id)
            .fetch_one(&mut *tx)
            .await?;
            if existing_payload_hash != payload_hash {
                anyhow::bail!("凭据统计 operation_id {} 已用于不同 payload", operation_id);
            }
            sqlx::query(
                "UPDATE credential_stats_delta_batches SET created_at = now() WHERE operation_id = $1",
            )
            .bind(&operation_id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(());
        }

        for chunk in rows.chunks(1_000) {
            let now = Utc::now();
            let mut builder = QueryBuilder::<Postgres>::new(
                r#"
                INSERT INTO credential_stats (
                    credential_id, success_count, selection_count, last_used_at, updated_at
                )
                "#,
            );
            builder.push_values(
                chunk.iter(),
                |mut row, &(credential_id, success_delta, selection_delta, ref last_used_at)| {
                    row.push_bind(credential_id)
                        .push_bind(success_delta)
                        .push_bind(selection_delta)
                        .push_bind(last_used_at.clone())
                        .push_bind(now.clone());
                },
            );
            builder.push(
                r#"
                ON CONFLICT (credential_id) DO UPDATE
                SET success_count = credential_stats.success_count + EXCLUDED.success_count,
                    selection_count = credential_stats.selection_count + EXCLUDED.selection_count,
                    last_used_at = CASE
                        WHEN EXCLUDED.last_used_at IS NULL THEN credential_stats.last_used_at
                        WHEN credential_stats.last_used_at IS NULL THEN EXCLUDED.last_used_at
                        WHEN EXCLUDED.last_used_at::timestamptz
                             >= credential_stats.last_used_at::timestamptz
                            THEN EXCLUDED.last_used_at
                        ELSE credential_stats.last_used_at
                    END,
                    updated_at = now()
                "#,
            );
            builder.build().execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn apply_credential_runtime_state_snapshots(
        &self,
        snapshots: &HashMap<u64, CredentialRuntimeStateSnapshot>,
    ) -> anyhow::Result<HashMap<u64, CredentialRuntimeStateCasOutcome>> {
        let mut outcomes = HashMap::with_capacity(snapshots.len());
        for (credential_id, snapshot) in snapshots {
            let outcome = self
                .save_credential_runtime_state_snapshot(*credential_id, snapshot)
                .await?;
            outcomes.insert(*credential_id, outcome);
        }
        Ok(outcomes)
    }

    #[cfg(test)]
    pub async fn record_credential_selection(&self, credential_id: u64) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO credential_stats (credential_id, selection_count, updated_at)
            VALUES ($1, 1, now())
            ON CONFLICT (credential_id) DO UPDATE
            SET selection_count = credential_stats.selection_count + 1,
                updated_at = now()
            "#,
        )
        .bind(credential_id as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    #[allow(dead_code)]
    pub async fn record_credential_success(
        &self,
        credential_id: u64,
        operation_id: Uuid,
    ) -> anyhow::Result<CredentialRuntimeStateRow> {
        Ok(self
            .record_credential_success_with_expected_generation(
                credential_id,
                operation_id,
                None,
                1,
            )
            .await?
            .state)
    }

    pub async fn record_credential_success_at_generation_with_count(
        &self,
        credential_id: u64,
        operation_id: Uuid,
        expected_generation: u64,
        success_count: u32,
    ) -> anyhow::Result<CredentialRuntimeStateMutationResult> {
        self.record_credential_success_with_expected_generation(
            credential_id,
            operation_id,
            Some(expected_generation),
            success_count,
        )
        .await
    }

    pub async fn record_credential_success_if_runtime_dirty_at_generation(
        &self,
        credential_id: u64,
        operation_id: Uuid,
        expected_generation: u64,
    ) -> anyhow::Result<CredentialRuntimeStateMutationResult> {
        let mut tx = self.pool.begin().await?;
        let credential_disabled = lock_active_credential_in_tx(&mut tx, credential_id).await?;
        sqlx::query(
            r#"
            INSERT INTO credential_runtime_state (
                credential_id, failure_count, refresh_failure_count,
                disabled_reason, warmup_remaining, generation, revision, updated_at
            )
            VALUES ($1, 0, 0, NULL, 0, 0, 0, now())
            ON CONFLICT (credential_id) DO NOTHING
            "#,
        )
        .bind(credential_id as i64)
        .execute(&mut *tx)
        .await?;

        let row = sqlx::query(
            r#"
            SELECT failure_count, refresh_failure_count, disabled_reason,
                   warmup_remaining, generation, revision
            FROM credential_runtime_state
            WHERE credential_id = $1
            FOR UPDATE
            "#,
        )
        .bind(credential_id as i64)
        .fetch_one(&mut *tx)
        .await?;
        let state = runtime_state_from_row(&row)?;
        if expected_generation < state.generation {
            tx.commit().await?;
            return Ok(CredentialRuntimeStateMutationResult {
                state,
                credential_disabled,
                applied: false,
            });
        }
        if expected_generation > state.generation {
            anyhow::bail!(
                "凭据 #{} 运行态 generation 超前：期望 {}，当前 {}",
                credential_id,
                expected_generation,
                state.generation
            );
        }

        let runtime_dirty = state.failure_count > 0
            || state.refresh_failure_count > 0
            || state.disabled_reason.is_some()
            || state.warmup_remaining > 0;
        if !runtime_dirty {
            tx.commit().await?;
            return Ok(CredentialRuntimeStateMutationResult {
                state,
                credential_disabled,
                applied: false,
            });
        }

        let (next_revision, credential_disabled) = match prepare_credential_runtime_mutation(
            &mut tx,
            credential_id,
            operation_id,
            "success",
            Some(expected_generation),
        )
        .await?
        {
            CredentialRuntimeMutationPreparation::Apply {
                next_revision,
                credential_disabled,
                ..
            } => (next_revision, credential_disabled),
            CredentialRuntimeMutationPreparation::Duplicate {
                state,
                credential_disabled,
            } => {
                tx.commit().await?;
                return Ok(CredentialRuntimeStateMutationResult {
                    state,
                    credential_disabled,
                    applied: true,
                });
            }
            CredentialRuntimeMutationPreparation::Stale {
                state,
                credential_disabled,
            } => {
                tx.commit().await?;
                return Ok(CredentialRuntimeStateMutationResult {
                    state,
                    credential_disabled,
                    applied: false,
                });
            }
        };
        let row = sqlx::query(
            r#"
            UPDATE credential_runtime_state
            SET failure_count = 0,
                refresh_failure_count = 0,
                warmup_remaining = GREATEST(credential_runtime_state.warmup_remaining - 1, 0),
                revision = credential_runtime_state.revision + 1,
                updated_at = now()
            WHERE credential_id = $1
            RETURNING failure_count, refresh_failure_count, disabled_reason,
                      warmup_remaining, generation, revision
            "#,
        )
        .bind(credential_id as i64)
        .fetch_one(&mut *tx)
        .await?;
        let state = runtime_state_from_row(&row)?;
        verify_runtime_mutation_revision(&state, next_revision)?;
        tx.commit().await?;
        Ok(CredentialRuntimeStateMutationResult {
            state,
            credential_disabled,
            applied: true,
        })
    }

    async fn record_credential_success_with_expected_generation(
        &self,
        credential_id: u64,
        operation_id: Uuid,
        expected_generation: Option<u64>,
        success_count: u32,
    ) -> anyhow::Result<CredentialRuntimeStateMutationResult> {
        let warmup_decrement = i32::try_from(success_count.max(1)).unwrap_or(i32::MAX);
        let mut tx = self.pool.begin().await?;
        let (next_revision, credential_disabled) = match prepare_credential_runtime_mutation(
            &mut tx,
            credential_id,
            operation_id,
            "success",
            expected_generation,
        )
        .await?
        {
            CredentialRuntimeMutationPreparation::Apply {
                next_revision,
                credential_disabled,
                ..
            } => (next_revision, credential_disabled),
            CredentialRuntimeMutationPreparation::Duplicate {
                state,
                credential_disabled,
            } => {
                tx.commit().await?;
                return Ok(CredentialRuntimeStateMutationResult {
                    state,
                    credential_disabled,
                    applied: true,
                });
            }
            CredentialRuntimeMutationPreparation::Stale {
                state,
                credential_disabled,
            } => {
                tx.commit().await?;
                return Ok(CredentialRuntimeStateMutationResult {
                    state,
                    credential_disabled,
                    applied: false,
                });
            }
        };
        let row = sqlx::query(
            r#"
            UPDATE credential_runtime_state
            SET failure_count = 0,
                refresh_failure_count = 0,
                warmup_remaining = GREATEST(
                    credential_runtime_state.warmup_remaining - $2,
                    0
                ),
                revision = credential_runtime_state.revision + 1,
                updated_at = now()
            WHERE credential_id = $1
            RETURNING failure_count, refresh_failure_count, disabled_reason,
                      warmup_remaining, generation, revision
            "#,
        )
        .bind(credential_id as i64)
        .bind(warmup_decrement)
        .fetch_one(&mut *tx)
        .await?;
        let state = runtime_state_from_row(&row)?;
        verify_runtime_mutation_revision(&state, next_revision)?;
        tx.commit().await?;
        Ok(CredentialRuntimeStateMutationResult {
            state,
            credential_disabled,
            applied: true,
        })
    }

    #[allow(dead_code)]
    pub async fn record_credential_api_failure(
        &self,
        credential_id: u64,
        operation_id: Uuid,
        last_used_at: &str,
        max_failures: u32,
    ) -> anyhow::Result<CredentialRuntimeStateRow> {
        Ok(self
            .record_credential_api_failure_with_expected_generation(
                credential_id,
                operation_id,
                None,
                last_used_at,
                max_failures,
            )
            .await?
            .state)
    }

    pub async fn record_credential_api_failure_at_generation(
        &self,
        credential_id: u64,
        operation_id: Uuid,
        expected_generation: u64,
        last_used_at: &str,
        max_failures: u32,
    ) -> anyhow::Result<CredentialRuntimeStateMutationResult> {
        self.record_credential_api_failure_with_expected_generation(
            credential_id,
            operation_id,
            Some(expected_generation),
            last_used_at,
            max_failures,
        )
        .await
    }

    async fn record_credential_api_failure_with_expected_generation(
        &self,
        credential_id: u64,
        operation_id: Uuid,
        expected_generation: Option<u64>,
        last_used_at: &str,
        max_failures: u32,
    ) -> anyhow::Result<CredentialRuntimeStateMutationResult> {
        let mut tx = self.pool.begin().await?;
        let (next_revision, mut credential_disabled) = match prepare_credential_runtime_mutation(
            &mut tx,
            credential_id,
            operation_id,
            "api_failure",
            expected_generation,
        )
        .await?
        {
            CredentialRuntimeMutationPreparation::Apply {
                next_revision,
                credential_disabled,
                ..
            } => (next_revision, credential_disabled),
            CredentialRuntimeMutationPreparation::Duplicate {
                state,
                credential_disabled,
            } => {
                tx.commit().await?;
                return Ok(CredentialRuntimeStateMutationResult {
                    state,
                    credential_disabled,
                    applied: true,
                });
            }
            CredentialRuntimeMutationPreparation::Stale {
                state,
                credential_disabled,
            } => {
                tx.commit().await?;
                return Ok(CredentialRuntimeStateMutationResult {
                    state,
                    credential_disabled,
                    applied: false,
                });
            }
        };
        upsert_last_used_at(&mut tx, credential_id, last_used_at).await?;
        let row = sqlx::query(
            r#"
            UPDATE credential_runtime_state
            SET failure_count = credential_runtime_state.failure_count + 1,
                disabled_reason = CASE
                    WHEN credential_runtime_state.failure_count + 1 >= $2
                        THEN 'TooManyFailures'
                    ELSE credential_runtime_state.disabled_reason
                END,
                revision = credential_runtime_state.revision + 1,
                updated_at = now()
            WHERE credential_id = $1
            RETURNING failure_count, refresh_failure_count, disabled_reason,
                      warmup_remaining, generation, revision
            "#,
        )
        .bind(credential_id as i64)
        .bind(max_failures as i32)
        .fetch_one(&mut *tx)
        .await?;
        let state = runtime_state_from_row(&row)?;
        verify_runtime_mutation_revision(&state, next_revision)?;
        if state.disabled_reason.as_deref() == Some("TooManyFailures") {
            persist_credential_disabled_flag_in_tx(&mut tx, credential_id, true).await?;
            credential_disabled = true;
        }
        tx.commit().await?;
        Ok(CredentialRuntimeStateMutationResult {
            state,
            credential_disabled,
            applied: true,
        })
    }

    #[allow(dead_code)]
    pub async fn record_credential_refresh_failure(
        &self,
        credential_id: u64,
        operation_id: Uuid,
        last_used_at: &str,
        max_failures: u32,
    ) -> anyhow::Result<CredentialRuntimeStateRow> {
        Ok(self
            .record_credential_refresh_failure_with_expected_generation(
                credential_id,
                operation_id,
                None,
                last_used_at,
                max_failures,
            )
            .await?
            .state)
    }

    #[cfg(test)]
    pub async fn record_credential_refresh_failure_at_generation(
        &self,
        credential_id: u64,
        operation_id: Uuid,
        expected_generation: u64,
        last_used_at: &str,
        max_failures: u32,
    ) -> anyhow::Result<CredentialRuntimeStateMutationResult> {
        self.record_credential_refresh_failure_with_expected_generation(
            credential_id,
            operation_id,
            Some(expected_generation),
            last_used_at,
            max_failures,
        )
        .await
    }

    async fn record_credential_refresh_failure_with_expected_generation(
        &self,
        credential_id: u64,
        operation_id: Uuid,
        expected_generation: Option<u64>,
        last_used_at: &str,
        max_failures: u32,
    ) -> anyhow::Result<CredentialRuntimeStateMutationResult> {
        let mut tx = self.pool.begin().await?;
        let (next_revision, mut credential_disabled) = match prepare_credential_runtime_mutation(
            &mut tx,
            credential_id,
            operation_id,
            "refresh_failure",
            expected_generation,
        )
        .await?
        {
            CredentialRuntimeMutationPreparation::Apply {
                next_revision,
                credential_disabled,
                ..
            } => (next_revision, credential_disabled),
            CredentialRuntimeMutationPreparation::Duplicate {
                state,
                credential_disabled,
            } => {
                tx.commit().await?;
                return Ok(CredentialRuntimeStateMutationResult {
                    state,
                    credential_disabled,
                    applied: true,
                });
            }
            CredentialRuntimeMutationPreparation::Stale {
                state,
                credential_disabled,
            } => {
                tx.commit().await?;
                return Ok(CredentialRuntimeStateMutationResult {
                    state,
                    credential_disabled,
                    applied: false,
                });
            }
        };
        upsert_last_used_at(&mut tx, credential_id, last_used_at).await?;
        let row = sqlx::query(
            r#"
            UPDATE credential_runtime_state
            SET refresh_failure_count = credential_runtime_state.refresh_failure_count + 1,
                disabled_reason = CASE
                    WHEN credential_runtime_state.refresh_failure_count + 1 >= $2
                        THEN 'TooManyRefreshFailures'
                    ELSE credential_runtime_state.disabled_reason
                END,
                revision = credential_runtime_state.revision + 1,
                updated_at = now()
            WHERE credential_id = $1
            RETURNING failure_count, refresh_failure_count, disabled_reason,
                      warmup_remaining, generation, revision
            "#,
        )
        .bind(credential_id as i64)
        .bind(max_failures as i32)
        .fetch_one(&mut *tx)
        .await?;
        let state = runtime_state_from_row(&row)?;
        verify_runtime_mutation_revision(&state, next_revision)?;
        if state.disabled_reason.as_deref() == Some("TooManyRefreshFailures") {
            persist_credential_disabled_flag_in_tx(&mut tx, credential_id, true).await?;
            credential_disabled = true;
        }
        tx.commit().await?;
        Ok(CredentialRuntimeStateMutationResult {
            state,
            credential_disabled,
            applied: true,
        })
    }

    pub async fn heal_credential_api_failures(
        &self,
        credential_id: u64,
    ) -> anyhow::Result<Option<CredentialRuntimeStateRow>> {
        let mut tx = self.pool.begin().await?;
        lock_active_credential_in_tx(&mut tx, credential_id).await?;
        let current_reason: Option<String> = sqlx::query_scalar(
            r#"
            SELECT disabled_reason
            FROM credential_runtime_state
            WHERE credential_id = $1
            FOR UPDATE
            "#,
        )
        .bind(credential_id as i64)
        .fetch_optional(&mut *tx)
        .await?
        .flatten();
        if current_reason.as_deref() != Some("TooManyFailures") {
            tx.rollback().await?;
            return Ok(None);
        }

        let row = sqlx::query(
            r#"
            UPDATE credential_runtime_state
            SET failure_count = 0,
                disabled_reason = NULL,
                generation = credential_runtime_state.generation + 1,
                revision = credential_runtime_state.revision + 1,
                updated_at = now()
            WHERE credential_id = $1 AND disabled_reason = 'TooManyFailures'
            RETURNING failure_count, refresh_failure_count, disabled_reason,
                      warmup_remaining, generation, revision
            "#,
        )
        .bind(credential_id as i64)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            tx.rollback().await?;
            return Ok(None);
        };
        persist_credential_disabled_flag_in_tx(&mut tx, credential_id, false).await?;
        let state = runtime_state_from_row(&row)?;
        tx.commit().await?;
        Ok(Some(state))
    }

    pub async fn patch_credential_runtime_state(
        &self,
        credential_id: u64,
        operation_id: Uuid,
        patch: &CredentialRuntimeStatePatch,
    ) -> anyhow::Result<CredentialRuntimeStateMutationResult> {
        self.patch_credential_runtime_state_with_kind(credential_id, operation_id, "patch", patch)
            .await
    }

    async fn patch_credential_runtime_state_with_kind(
        &self,
        credential_id: u64,
        operation_id: Uuid,
        mutation_kind: &str,
        patch: &CredentialRuntimeStatePatch,
    ) -> anyhow::Result<CredentialRuntimeStateMutationResult> {
        validate_credential_runtime_state_patch(patch)?;
        let mut tx = self.pool.begin().await?;
        let result = Self::patch_credential_runtime_state_in_tx(
            &mut tx,
            credential_id,
            operation_id,
            mutation_kind,
            patch,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }

    async fn patch_credential_runtime_state_in_tx(
        tx: &mut Transaction<'_, Postgres>,
        credential_id: u64,
        operation_id: Uuid,
        mutation_kind: &str,
        patch: &CredentialRuntimeStatePatch,
    ) -> anyhow::Result<CredentialRuntimeStateMutationResult> {
        let failure_count = runtime_state_count_to_i32("failure_count", patch.failure_count)?;
        let refresh_failure_count =
            runtime_state_count_to_i32("refresh_failure_count", patch.refresh_failure_count)?;
        let warmup_remaining =
            runtime_state_count_to_i32("warmup_remaining", patch.warmup_remaining)?;
        let (update_disabled_reason, disabled_reason) = match &patch.disabled_reason {
            CredentialRuntimeDisabledReasonPatch::Preserve => (false, None),
            CredentialRuntimeDisabledReasonPatch::Set(reason) => (true, Some(reason.as_str())),
            CredentialRuntimeDisabledReasonPatch::Clear => (true, None),
        };

        let (next_revision, current_generation, current_credential_disabled) =
            match prepare_credential_runtime_mutation(
                tx,
                credential_id,
                operation_id,
                mutation_kind,
                patch.expected_generation,
            )
            .await?
            {
                CredentialRuntimeMutationPreparation::Apply {
                    next_revision,
                    current_generation,
                    credential_disabled,
                } => (next_revision, current_generation, credential_disabled),
                CredentialRuntimeMutationPreparation::Duplicate {
                    state,
                    credential_disabled,
                } => {
                    return Ok(CredentialRuntimeStateMutationResult {
                        state,
                        credential_disabled,
                        applied: true,
                    });
                }
                CredentialRuntimeMutationPreparation::Stale {
                    state,
                    credential_disabled,
                } => {
                    return Ok(CredentialRuntimeStateMutationResult {
                        state,
                        credential_disabled,
                        applied: false,
                    });
                }
            };
        let next_generation = if patch.advance_generation {
            current_generation
                .checked_add(1)
                .filter(|generation| i64::try_from(*generation).is_ok())
                .ok_or_else(|| anyhow::anyhow!("凭据运行态 generation 已达到 PgSQL BIGINT 上限"))?
        } else {
            current_generation
        };
        if let Some(last_used_at) = patch.last_used_at.as_deref() {
            upsert_last_used_at(tx, credential_id, last_used_at).await?;
        }
        let row = sqlx::query(
            r#"
            UPDATE credential_runtime_state
            SET failure_count = COALESCE($2, credential_runtime_state.failure_count),
                refresh_failure_count = COALESCE($3, credential_runtime_state.refresh_failure_count),
                disabled_reason = CASE
                    WHEN $4 THEN $5
                    ELSE credential_runtime_state.disabled_reason
                END,
                warmup_remaining = COALESCE($6, credential_runtime_state.warmup_remaining),
                generation = credential_runtime_state.generation
                    + CASE WHEN $7 THEN 1 ELSE 0 END,
                revision = credential_runtime_state.revision + 1,
                updated_at = now()
            WHERE credential_id = $1
            RETURNING failure_count, refresh_failure_count, disabled_reason,
                      warmup_remaining, generation, revision
            "#,
        )
        .bind(credential_id as i64)
        .bind(failure_count)
        .bind(refresh_failure_count)
        .bind(update_disabled_reason)
        .bind(disabled_reason)
        .bind(warmup_remaining)
        .bind(patch.advance_generation)
        .fetch_one(&mut **tx)
        .await?;
        let state = runtime_state_from_row(&row)?;
        verify_runtime_mutation_revision(&state, next_revision)?;
        verify_runtime_mutation_generation(&state, next_generation)?;
        let credential_disabled = if let Some(disabled) = patch.credential_disabled {
            persist_credential_disabled_flag_in_tx(tx, credential_id, disabled).await?;
            disabled
        } else {
            current_credential_disabled
        };
        Ok(CredentialRuntimeStateMutationResult {
            state,
            credential_disabled,
            applied: true,
        })
    }

    #[allow(dead_code)]
    pub async fn mark_credential_disabled(
        &self,
        credential_id: u64,
        operation_id: Uuid,
        reason: &str,
        failure_count: Option<u32>,
        refresh_failure_count: Option<u32>,
        last_used_at: &str,
    ) -> anyhow::Result<CredentialRuntimeStateMutationResult> {
        self.mark_credential_disabled_with_expected_generation(
            credential_id,
            operation_id,
            None,
            reason,
            CredentialRuntimeFailureCounts {
                failure_count,
                refresh_failure_count,
            },
            last_used_at,
        )
        .await
    }

    pub async fn mark_credential_disabled_at_generation(
        &self,
        credential_id: u64,
        operation_id: Uuid,
        expected_generation: u64,
        reason: &str,
        failure_counts: CredentialRuntimeFailureCounts,
        last_used_at: &str,
    ) -> anyhow::Result<CredentialRuntimeStateMutationResult> {
        self.mark_credential_disabled_with_expected_generation(
            credential_id,
            operation_id,
            Some(expected_generation),
            reason,
            failure_counts,
            last_used_at,
        )
        .await
    }

    async fn mark_credential_disabled_with_expected_generation(
        &self,
        credential_id: u64,
        operation_id: Uuid,
        expected_generation: Option<u64>,
        reason: &str,
        failure_counts: CredentialRuntimeFailureCounts,
        last_used_at: &str,
    ) -> anyhow::Result<CredentialRuntimeStateMutationResult> {
        self.patch_credential_runtime_state_with_kind(
            credential_id,
            operation_id,
            "disable",
            &CredentialRuntimeStatePatch {
                failure_count: failure_counts.failure_count,
                refresh_failure_count: failure_counts.refresh_failure_count,
                disabled_reason: CredentialRuntimeDisabledReasonPatch::Set(reason.to_string()),
                credential_disabled: Some(true),
                last_used_at: Some(last_used_at.to_string()),
                expected_generation,
                ..Default::default()
            },
        )
        .await
    }

    #[allow(dead_code)]
    pub async fn update_credential_last_used_at(
        &self,
        credential_id: u64,
        last_used_at: &str,
    ) -> anyhow::Result<()> {
        validate_rfc3339_timestamp("last_used_at", last_used_at)?;
        sqlx::query(
            r#"
            INSERT INTO credential_stats (credential_id, success_count, last_used_at, updated_at)
            VALUES ($1, 0, $2, now())
            ON CONFLICT (credential_id) DO UPDATE
            SET last_used_at = CASE
                    WHEN credential_stats.last_used_at IS NULL THEN EXCLUDED.last_used_at
                    WHEN EXCLUDED.last_used_at::timestamptz
                         >= credential_stats.last_used_at::timestamptz
                        THEN EXCLUDED.last_used_at
                    ELSE credential_stats.last_used_at
                END,
                updated_at = now()
            "#,
        )
        .bind(credential_id as i64)
        .bind(last_used_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn save_pricing_status(&self, status: &PricingStatus) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        let incoming_models: Vec<String> = status
            .models
            .iter()
            .map(|item| item.model.clone())
            .collect();
        for item in &status.models {
            let item_source = item.source.as_deref().unwrap_or(&status.source);
            let item_source_url = if item_source == MANUAL_PRICING_SOURCE {
                "manual"
            } else {
                &status.source_url
            };
            sqlx::query(
                r#"
                INSERT INTO model_pricing (
                    model, input_cost_per_token, output_cost_per_token,
                    cache_creation_input_token_cost, cache_read_input_token_cost,
                    source, source_url, last_synced_at, last_error, updated_at
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, now())
                ON CONFLICT (model) DO UPDATE
                SET input_cost_per_token = EXCLUDED.input_cost_per_token,
                    output_cost_per_token = EXCLUDED.output_cost_per_token,
                    cache_creation_input_token_cost = EXCLUDED.cache_creation_input_token_cost,
                    cache_read_input_token_cost = EXCLUDED.cache_read_input_token_cost,
                    source = EXCLUDED.source,
                    source_url = EXCLUDED.source_url,
                    last_synced_at = EXCLUDED.last_synced_at,
                    last_error = EXCLUDED.last_error,
                    updated_at = now()
                "#,
            )
            .bind(&item.model)
            .bind(item.pricing.input_cost_per_token)
            .bind(item.pricing.output_cost_per_token)
            .bind(item.pricing.cache_creation_input_token_cost)
            .bind(item.pricing.cache_read_input_token_cost)
            .bind(item_source)
            .bind(item_source_url)
            .bind(&status.last_synced_at)
            .bind(&status.last_error)
            .execute(&mut *tx)
            .await?;
        }
        if incoming_models.is_empty() {
            sqlx::query("DELETE FROM model_pricing WHERE source <> $1")
                .bind(MANUAL_PRICING_SOURCE)
                .execute(&mut *tx)
                .await?;
        } else {
            sqlx::query(
                "DELETE FROM model_pricing WHERE source <> $1 AND model <> ALL($2::text[])",
            )
            .bind(MANUAL_PRICING_SOURCE)
            .bind(incoming_models)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            r#"
            INSERT INTO model_pricing_sync_status (
                id, source, source_url, last_synced_at, last_error, model_count, updated_at
            )
            VALUES ('default', $1, $2, $3, $4, $5, now())
            ON CONFLICT (id) DO UPDATE
            SET source = EXCLUDED.source,
                source_url = EXCLUDED.source_url,
                last_synced_at = EXCLUDED.last_synced_at,
                last_error = EXCLUDED.last_error,
                model_count = EXCLUDED.model_count,
                updated_at = now()
            "#,
        )
        .bind(&status.source)
        .bind(&status.source_url)
        .bind(&status.last_synced_at)
        .bind(&status.last_error)
        .bind(status.model_count as i32)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    #[cfg(test)]
    pub async fn load_model_pricing(&self) -> anyhow::Result<HashMap<String, ModelPricing>> {
        let rows = sqlx::query(
            r#"
            SELECT model, input_cost_per_token, output_cost_per_token,
                   cache_creation_input_token_cost, cache_read_input_token_cost
            FROM model_pricing
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        let mut prices = HashMap::with_capacity(rows.len());
        for row in rows {
            prices.insert(
                row.try_get("model")?,
                ModelPricing {
                    input_cost_per_token: row.try_get("input_cost_per_token")?,
                    output_cost_per_token: row.try_get("output_cost_per_token")?,
                    cache_creation_input_token_cost: row
                        .try_get("cache_creation_input_token_cost")?,
                    cache_read_input_token_cost: row.try_get("cache_read_input_token_cost")?,
                },
            );
        }
        Ok(prices)
    }

    pub async fn load_pricing_status(&self) -> anyhow::Result<Option<PricingStatus>> {
        let rows = sqlx::query(
            r#"
            SELECT model, input_cost_per_token, output_cost_per_token,
                   cache_creation_input_token_cost, cache_read_input_token_cost,
                   source, source_url, last_synced_at, last_error
            FROM model_pricing
            ORDER BY model ASC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        if rows.is_empty() {
            return Ok(None);
        }

        let status_row = sqlx::query(
            r#"
            SELECT source, source_url, last_synced_at, last_error
            FROM model_pricing_sync_status
            WHERE id = 'default'
            "#,
        )
        .fetch_optional(&self.pool)
        .await?;
        let (source, source_url, last_synced_at, last_error): (
            String,
            String,
            Option<String>,
            Option<String>,
        ) = if let Some(row) = status_row {
            (
                row.try_get("source")?,
                row.try_get("source_url")?,
                row.try_get("last_synced_at")?,
                row.try_get("last_error")?,
            )
        } else {
            (
                rows[0].try_get("source")?,
                rows[0].try_get("source_url")?,
                rows[0].try_get("last_synced_at")?,
                rows[0].try_get("last_error")?,
            )
        };
        let mut models = Vec::with_capacity(rows.len());
        for row in rows {
            models.push(ModelPriceItem {
                model: row.try_get("model")?,
                pricing: ModelPricing {
                    input_cost_per_token: row.try_get("input_cost_per_token")?,
                    output_cost_per_token: row.try_get("output_cost_per_token")?,
                    cache_creation_input_token_cost: row
                        .try_get("cache_creation_input_token_cost")?,
                    cache_read_input_token_cost: row.try_get("cache_read_input_token_cost")?,
                },
                source: Some(row.try_get("source")?),
            });
        }

        Ok(Some(PricingStatus {
            available: true,
            source,
            source_url,
            model_count: models.len(),
            last_synced_at,
            last_error,
            models,
        }))
    }

    pub async fn save_model_capabilities_status(
        &self,
        status: &ModelCapabilitiesStatus,
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        let incoming_models: Vec<String> = status
            .models
            .iter()
            .map(|item| item.model.clone())
            .collect();
        for item in &status.models {
            sqlx::query(
                r#"
                INSERT INTO model_capabilities (
                    model, display_name, description, max_input_tokens, max_output_tokens,
                    supports_prompt_caching, supported_input_types, source,
                    last_synced_at, last_error, updated_at
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, now())
                ON CONFLICT (model) DO UPDATE
                SET display_name = EXCLUDED.display_name,
                    description = EXCLUDED.description,
                    max_input_tokens = EXCLUDED.max_input_tokens,
                    max_output_tokens = EXCLUDED.max_output_tokens,
                    supports_prompt_caching = EXCLUDED.supports_prompt_caching,
                    supported_input_types = EXCLUDED.supported_input_types,
                    source = EXCLUDED.source,
                    last_synced_at = EXCLUDED.last_synced_at,
                    last_error = EXCLUDED.last_error,
                    updated_at = now()
                "#,
            )
            .bind(&item.model)
            .bind(&item.display_name)
            .bind(&item.description)
            .bind(item.max_input_tokens)
            .bind(item.max_output_tokens)
            .bind(item.supports_prompt_caching)
            .bind(&item.supported_input_types)
            .bind(item.source.as_deref().unwrap_or(&status.source))
            .bind(&status.last_synced_at)
            .bind(&status.last_error)
            .execute(&mut *tx)
            .await?;
        }
        if incoming_models.is_empty() {
            sqlx::query("DELETE FROM model_capabilities WHERE source <> $1")
                .bind(MANUAL_SOURCE)
                .execute(&mut *tx)
                .await?;
        } else {
            sqlx::query(
                "DELETE FROM model_capabilities WHERE source <> $1 AND model <> ALL($2::text[])",
            )
            .bind(MANUAL_SOURCE)
            .bind(incoming_models)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            r#"
            INSERT INTO model_capabilities_sync_status (
                id, source, last_synced_at, last_error, model_count,
                reasoning_fields, reasoning_cohort_keys, reasoning_cohort_complete,
                reasoning_contract_version, reasoning_invalid_models, updated_at
            )
            VALUES ('default', $1, $2, $3, $4, $5, $6, $7, $8, $9, now())
            ON CONFLICT (id) DO UPDATE
            SET source = EXCLUDED.source,
                last_synced_at = EXCLUDED.last_synced_at,
                last_error = EXCLUDED.last_error,
                model_count = EXCLUDED.model_count,
                reasoning_fields = EXCLUDED.reasoning_fields,
                reasoning_cohort_keys = EXCLUDED.reasoning_cohort_keys,
                reasoning_cohort_complete = EXCLUDED.reasoning_cohort_complete,
                reasoning_contract_version = EXCLUDED.reasoning_contract_version,
                reasoning_invalid_models = EXCLUDED.reasoning_invalid_models,
                updated_at = now()
            "#,
        )
        .bind(&status.source)
        .bind(&status.last_synced_at)
        .bind(&status.last_error)
        .bind(status.model_count as i32)
        .bind(serde_json::to_value(&status.reasoning_fields)?)
        .bind(serde_json::to_value(
            &status.reasoning_capability_cohort_keys,
        )?)
        .bind(status.reasoning_capability_cohort_complete)
        .bind(status.reasoning_capability_contract_version as i32)
        .bind(serde_json::to_value(&status.reasoning_invalid_models)?)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn load_model_capabilities_status(
        &self,
    ) -> anyhow::Result<Option<ModelCapabilitiesStatus>> {
        let rows = sqlx::query(
            r#"
            SELECT model, display_name, description, max_input_tokens, max_output_tokens,
                   supports_prompt_caching, supported_input_types, source,
                   last_synced_at, last_error
            FROM model_capabilities
            ORDER BY model ASC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        if rows.is_empty() {
            return Ok(None);
        }

        let status_row = sqlx::query(
            r#"
            SELECT source, last_synced_at, last_error, reasoning_fields,
                   reasoning_cohort_keys, reasoning_cohort_complete,
                   reasoning_contract_version, reasoning_invalid_models
            FROM model_capabilities_sync_status
            WHERE id = 'default'
            "#,
        )
        .fetch_optional(&self.pool)
        .await?;
        let (
            source,
            last_synced_at,
            last_error,
            reasoning_fields,
            reasoning_capability_cohort_keys,
            reasoning_capability_cohort_complete,
            reasoning_capability_contract_version,
            reasoning_invalid_models,
        ): (
            String,
            Option<String>,
            Option<String>,
            BTreeMap<String, KiroReasoningFieldCapability>,
            Vec<KiroModelCapabilityCohortKey>,
            bool,
            u32,
            Vec<String>,
        ) = if let Some(row) = status_row {
            (
                row.try_get("source")?,
                row.try_get("last_synced_at")?,
                row.try_get("last_error")?,
                serde_json::from_value(row.try_get("reasoning_fields")?).unwrap_or_default(),
                serde_json::from_value(row.try_get("reasoning_cohort_keys")?).unwrap_or_default(),
                row.try_get("reasoning_cohort_complete")?,
                row.try_get::<i32, _>("reasoning_contract_version")?.max(0) as u32,
                serde_json::from_value(row.try_get("reasoning_invalid_models")?)
                    .unwrap_or_default(),
            )
        } else {
            (
                rows[0].try_get("source")?,
                rows[0].try_get("last_synced_at")?,
                rows[0].try_get("last_error")?,
                std::collections::BTreeMap::new(),
                Vec::new(),
                false,
                0,
                Vec::new(),
            )
        };
        let mut models = Vec::with_capacity(rows.len());
        for row in rows {
            models.push(ModelCapabilityItem {
                model: row.try_get("model")?,
                display_name: row.try_get("display_name")?,
                description: row.try_get("description")?,
                max_input_tokens: row.try_get("max_input_tokens")?,
                max_output_tokens: row.try_get("max_output_tokens")?,
                supports_prompt_caching: row.try_get("supports_prompt_caching")?,
                supported_input_types: row.try_get("supported_input_types")?,
                source: Some(row.try_get("source")?),
            });
        }

        Ok(Some(ModelCapabilitiesStatus {
            available: true,
            source,
            model_count: models.len(),
            last_synced_at,
            last_error,
            models,
            reasoning_fields,
            reasoning_capability_cohort_keys,
            reasoning_capability_cohort_complete,
            reasoning_capability_contract_version,
            reasoning_invalid_models,
        }))
    }

    pub async fn save_manual_model(
        &self,
        item: &ModelCapabilityItem,
        pricing: Option<ModelPricing>,
        clear_pricing: bool,
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"
            INSERT INTO model_capabilities (
                model, display_name, description, max_input_tokens, max_output_tokens,
                supports_prompt_caching, supported_input_types, source,
                last_synced_at, last_error, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NULL, NULL, now())
            ON CONFLICT (model) DO UPDATE
            SET display_name = EXCLUDED.display_name,
                description = EXCLUDED.description,
                max_input_tokens = EXCLUDED.max_input_tokens,
                max_output_tokens = EXCLUDED.max_output_tokens,
                supports_prompt_caching = EXCLUDED.supports_prompt_caching,
                supported_input_types = EXCLUDED.supported_input_types,
                source = EXCLUDED.source,
                last_error = NULL,
                updated_at = now()
            "#,
        )
        .bind(&item.model)
        .bind(&item.display_name)
        .bind(&item.description)
        .bind(item.max_input_tokens)
        .bind(item.max_output_tokens)
        .bind(item.supports_prompt_caching)
        .bind(&item.supported_input_types)
        .bind(item.source.as_deref().unwrap_or(MANUAL_SOURCE))
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "UPDATE model_capabilities_sync_status SET reasoning_fields = reasoning_fields - $1, updated_at = now() WHERE id = 'default'",
        )
        .bind(&item.model)
        .execute(&mut *tx)
        .await?;

        if let Some(pricing) = pricing {
            sqlx::query(
                r#"
                INSERT INTO model_pricing (
                    model, input_cost_per_token, output_cost_per_token,
                    cache_creation_input_token_cost, cache_read_input_token_cost,
                    source, source_url, last_synced_at, last_error, updated_at
                )
                VALUES ($1, $2, $3, $4, $5, $6, 'manual', NULL, NULL, now())
                ON CONFLICT (model) DO UPDATE
                SET input_cost_per_token = EXCLUDED.input_cost_per_token,
                    output_cost_per_token = EXCLUDED.output_cost_per_token,
                    cache_creation_input_token_cost = EXCLUDED.cache_creation_input_token_cost,
                    cache_read_input_token_cost = EXCLUDED.cache_read_input_token_cost,
                    source = EXCLUDED.source,
                    source_url = EXCLUDED.source_url,
                    last_error = NULL,
                    updated_at = now()
                "#,
            )
            .bind(&item.model)
            .bind(pricing.input_cost_per_token)
            .bind(pricing.output_cost_per_token)
            .bind(pricing.cache_creation_input_token_cost)
            .bind(pricing.cache_read_input_token_cost)
            .bind(MANUAL_PRICING_SOURCE)
            .execute(&mut *tx)
            .await?;
        } else if clear_pricing {
            sqlx::query("DELETE FROM model_pricing WHERE model = $1 AND source = $2")
                .bind(&item.model)
                .bind(MANUAL_PRICING_SOURCE)
                .execute(&mut *tx)
                .await?;
        }

        tx.commit().await?;
        Ok(())
    }

    pub async fn delete_manual_model(&self, model: &str) -> anyhow::Result<bool> {
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query("DELETE FROM model_capabilities WHERE model = $1 AND source = $2")
            .bind(model)
            .bind(MANUAL_SOURCE)
            .execute(&mut *tx)
            .await?;
        if result.rows_affected() > 0 {
            sqlx::query("DELETE FROM model_pricing WHERE model = $1 AND source = $2")
                .bind(model)
                .bind(MANUAL_PRICING_SOURCE)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn record_admin_audit_log(
        &self,
        actor: &str,
        action: &str,
        object_type: &str,
        object_id: Option<&str>,
        success: bool,
        error_message: Option<&str>,
        detail: serde_json::Value,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO admin_audit_logs (
                actor, action, object_type, object_id, success, error_message, detail, created_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, now())
            "#,
        )
        .bind(actor)
        .bind(action)
        .bind(object_type)
        .bind(object_id)
        .bind(success)
        .bind(error_message)
        .bind(detail)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn query_admin_audit_logs(
        &self,
        page: usize,
        limit: usize,
    ) -> anyhow::Result<AdminAuditLogPage> {
        let page = page.max(1);
        let limit = if limit == 0 { 20 } else { limit.min(200) };
        let offset = page.saturating_sub(1).saturating_mul(limit);
        let rows = sqlx::query(
            r#"
            SELECT id, created_at, actor, action, object_type, object_id,
                   success, error_message, detail
            FROM admin_audit_logs
            ORDER BY created_at DESC, id DESC
            OFFSET $1
            LIMIT $2
            "#,
        )
        .bind(usize_to_i64(offset))
        .bind(usize_to_i64(limit.saturating_add(1)))
        .fetch_all(&self.pool)
        .await?;
        let has_next = rows.len() > limit;
        let records = rows
            .into_iter()
            .take(limit)
            .map(|row| {
                let created_at: DateTime<Utc> = row.try_get("created_at")?;
                Ok(AdminAuditLogRow {
                    id: row.try_get("id")?,
                    created_at: created_at.to_rfc3339(),
                    actor: row.try_get("actor")?,
                    action: row.try_get("action")?,
                    object_type: row.try_get("object_type")?,
                    object_id: row.try_get("object_id")?,
                    success: row.try_get("success")?,
                    error_message: row.try_get("error_message")?,
                    detail: row.try_get("detail")?,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(AdminAuditLogPage {
            page,
            limit,
            has_next,
            records,
        })
    }

    pub async fn record_credential_event(
        &self,
        credential_id: Option<u64>,
        event_type: &str,
        reason: Option<&str>,
        detail: serde_json::Value,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO credential_events (credential_id, event_type, reason, detail, created_at)
            VALUES ($1, $2, $3, $4, now())
            "#,
        )
        .bind(credential_id.map(|id| id as i64))
        .bind(event_type)
        .bind(reason)
        .bind(detail)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

async fn execute_sql_statements(
    conn: &mut sqlx::pool::PoolConnection<Postgres>,
    sql: &str,
) -> anyhow::Result<()> {
    for statement in sql.split(";") {
        let statement = statement.trim();
        if !statement.is_empty() {
            sqlx::query(statement).execute(&mut **conn).await?;
        }
    }
    Ok(())
}

async fn usage_index_exists(
    tx: &mut Transaction<'_, Postgres>,
    definition: &UsageIndexDefinition,
) -> anyhow::Result<bool> {
    let exists: bool = sqlx::query_scalar(
        r#"
        SELECT EXISTS (
            SELECT 1
            FROM pg_indexes
            WHERE schemaname = current_schema()
              AND tablename = $1
              AND indexname = $2
        )
        "#,
    )
    .bind(definition.table)
    .bind(definition.name)
    .fetch_one(&mut **tx)
    .await?;
    Ok(exists)
}

async fn relation_size_bytes(
    tx: &mut Transaction<'_, Postgres>,
    table: &str,
) -> anyhow::Result<i64> {
    let size: i64 = sqlx::query_scalar("SELECT pg_total_relation_size($1::regclass)::bigint")
        .bind(table)
        .fetch_one(&mut **tx)
        .await?;
    Ok(size)
}

async fn ensure_usage_indexes_if_startup_safe(
    tx: &mut Transaction<'_, Postgres>,
) -> anyhow::Result<()> {
    let mut skipped = Vec::new();
    let mut sizes = HashMap::new();

    for definition in USAGE_INDEX_DEFINITIONS {
        if usage_index_exists(tx, definition).await? {
            continue;
        }

        let table_size = if let Some(size) = sizes.get(definition.table) {
            *size
        } else {
            let size = relation_size_bytes(tx, definition.table).await?;
            sizes.insert(definition.table, size);
            size
        };

        if table_size > USAGE_INDEX_STARTUP_MAX_BYTES {
            skipped.push(format!(
                "{} on {} ({} bytes)",
                definition.name, definition.table, table_size
            ));
            continue;
        }

        sqlx::query(definition.sql).execute(&mut **tx).await?;
    }

    if !skipped.is_empty() {
        tracing::warn!(
            skipped_indexes = ?skipped,
            max_startup_bytes = USAGE_INDEX_STARTUP_MAX_BYTES,
            "跳过 usage 大表索引启动创建；如需补齐索引，请低峰期显式执行 maintenance usage-indexes"
        );
    }

    Ok(())
}

async fn run_usage_exclusive_versioned_migration(
    conn: &mut sqlx::pool::PoolConnection<Postgres>,
    version: &str,
    sql: &str,
) -> anyhow::Result<()> {
    let mut tx = conn.begin().await?;
    acquire_usage_cleanup_commit_guard(&mut tx).await?;
    run_versioned_migration_in_tx(&mut tx, version, sql).await?;
    tx.commit().await?;
    Ok(())
}

async fn run_versioned_migration_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    version: &str,
    sql: &str,
) -> anyhow::Result<()> {
    let checksum = sha256_hex(sql);
    let applied_checksum: Option<String> =
        sqlx::query_scalar("SELECT checksum FROM schema_migrations WHERE version = $1")
            .bind(version)
            .fetch_optional(&mut **tx)
            .await?;
    if let Some(applied_checksum) = applied_checksum {
        if applied_checksum != checksum {
            anyhow::bail!("schema migration {version} checksum mismatch");
        }
        return Ok(());
    }

    execute_sql_statements_in_tx(tx, sql).await?;
    sqlx::query(
        r#"
        INSERT INTO schema_migrations (version, checksum, applied_at)
        VALUES ($1, $2, now())
        "#,
    )
    .bind(version)
    .bind(checksum)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn execute_sql_statements_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    sql: &str,
) -> anyhow::Result<()> {
    for statement in sql.split(";") {
        let statement = statement.trim();
        if !statement.is_empty() {
            sqlx::query(statement).execute(&mut **tx).await?;
        }
    }
    Ok(())
}

async fn repair_active_credential_hashes_in_tx(
    tx: &mut Transaction<'_, Postgres>,
) -> anyhow::Result<()> {
    let rows = sqlx::query(
        r#"
        SELECT id, api_key_hash, refresh_token_hash, data
        FROM credentials
        WHERE deleted_at IS NULL
          AND (
              (
                  api_key_hash IS NULL
                  AND COALESCE(data->>'kiroApiKey', data->>'kiro_api_key') IS NOT NULL
              )
              OR (
                  refresh_token_hash IS NULL
                  AND COALESCE(data->>'refreshToken', data->>'refresh_token') IS NOT NULL
              )
          )
        ORDER BY id ASC
        FOR UPDATE
        "#,
    )
    .fetch_all(&mut **tx)
    .await?;

    for row in rows {
        let credential_id: i64 = row.try_get("id")?;
        let existing_api_key_hash: Option<String> = row.try_get("api_key_hash")?;
        let existing_refresh_token_hash: Option<String> = row.try_get("refresh_token_hash")?;
        let data: serde_json::Value = row.try_get("data")?;
        let mut credential: KiroCredentials = serde_json::from_value(data)?;
        credential.id = Some(credential_id.max(0) as u64);
        credential.canonicalize_auth_method();
        credential.normalize_api_key_defaults();
        credential.normalize_external_idp_defaults();
        let (auth_kind, api_key_hash, refresh_token_hash) = credential_hash_columns(&credential);

        if let Some(api_key_hash) = api_key_hash
            .as_deref()
            .filter(|_| existing_api_key_hash.is_none())
        {
            let conflicting_id: Option<i64> = sqlx::query_scalar(
                r#"
                    SELECT id
                    FROM credentials
                    WHERE deleted_at IS NULL
                      AND id <> $1
                      AND api_key_hash = $2
                    LIMIT 1
                    "#,
            )
            .bind(credential_id)
            .bind(api_key_hash)
            .fetch_optional(&mut **tx)
            .await?;
            if let Some(conflicting_id) = conflicting_id {
                anyhow::bail!(
                    "凭据 #{} 与 #{} 的 kiroApiKey 重复，无法回填 hash",
                    credential_id,
                    conflicting_id
                );
            }
        }
        if let Some(refresh_token_hash) = refresh_token_hash
            .as_deref()
            .filter(|_| existing_refresh_token_hash.is_none())
        {
            let conflicting_id: Option<i64> = sqlx::query_scalar(
                r#"
                    SELECT id
                    FROM credentials
                    WHERE deleted_at IS NULL
                      AND id <> $1
                      AND refresh_token_hash = $2
                    LIMIT 1
                    "#,
            )
            .bind(credential_id)
            .bind(refresh_token_hash)
            .fetch_optional(&mut **tx)
            .await?;
            if let Some(conflicting_id) = conflicting_id {
                anyhow::bail!(
                    "凭据 #{} 与 #{} 的 refreshToken 重复，无法回填 hash",
                    credential_id,
                    conflicting_id
                );
            }
        }

        sqlx::query(
            r#"
            UPDATE credentials
            SET auth_kind = $2,
                api_key_hash = COALESCE(credentials.api_key_hash, $3),
                refresh_token_hash = COALESCE(credentials.refresh_token_hash, $4)
            WHERE id = $1 AND deleted_at IS NULL
            "#,
        )
        .bind(credential_id)
        .bind(auth_kind)
        .bind(api_key_hash)
        .bind(refresh_token_hash)
        .execute(&mut **tx)
        .await
        .map_err(duplicate_credential_message)?;
    }
    Ok(())
}

#[derive(Debug, Clone, Default)]
pub struct CredentialStatsRow {
    pub success_count: u64,
    pub selection_count: u64,
    pub last_used_at: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct CredentialStatsDeltaRow {
    pub success_delta: u64,
    pub selection_delta: u64,
    pub last_used_at: Option<String>,
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct CredentialRefreshFieldsPatch {
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub profile_arn: Option<String>,
    pub expires_at: Option<String>,
    pub scopes: Option<String>,
}

#[derive(Debug, Clone)]
#[must_use = "credential upsert CAS conflicts must be handled explicitly"]
pub enum CredentialUpsertCasOutcome {
    Applied(KiroCredentials),
    Conflict { current: KiroCredentials },
}

#[derive(Debug, Clone)]
#[must_use = "credential/runtime CAS conflicts must be handled explicitly"]
pub enum CredentialWithRuntimePatchCasOutcome {
    Applied {
        credential: KiroCredentials,
        runtime: CredentialRuntimeStateMutationResult,
    },
    Conflict {
        current: KiroCredentials,
    },
}

#[derive(Clone, PartialEq, Eq)]
pub struct CredentialRefreshExpectedContext {
    refresh_token_hash: String,
    access_token: Option<String>,
    auth_method: Option<String>,
    provider: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    token_endpoint: Option<String>,
    scopes: Option<String>,
}

impl CredentialRefreshExpectedContext {
    pub fn from_credentials(credentials: &KiroCredentials) -> anyhow::Result<Self> {
        let mut credentials = credentials.clone();
        credentials.canonicalize_auth_method();
        let refresh_token = credentials
            .refresh_token
            .as_deref()
            .filter(|token| !token.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("凭据 refreshToken CAS 缺少原 refreshToken"))?;
        Ok(Self {
            refresh_token_hash: sha256_hex(refresh_token),
            access_token: credentials.access_token,
            auth_method: credentials.auth_method,
            provider: credentials.provider,
            client_id: credentials.client_id,
            client_secret: credentials.client_secret,
            token_endpoint: credentials.token_endpoint,
            scopes: credentials.scopes,
        })
    }
}

#[derive(Debug, Clone)]
#[must_use = "credential refresh field CAS conflicts must be handled explicitly"]
pub enum CredentialRefreshFieldsCasOutcome {
    Applied(KiroCredentials),
    Conflict { current: Option<KiroCredentials> },
}

#[derive(Debug, Clone)]
pub struct CredentialRuntimeStateSnapshot {
    pub state: CredentialRuntimeStateRow,
    pub expected_revision: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CredentialRuntimeStateRow {
    pub failure_count: u32,
    pub refresh_failure_count: u32,
    pub disabled_reason: Option<String>,
    pub warmup_remaining: u32,
    pub generation: u64,
    pub revision: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum CredentialRuntimeDisabledReasonPatch {
    #[default]
    Preserve,
    Set(String),
    Clear,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CredentialRuntimeStatePatch {
    pub failure_count: Option<u32>,
    pub refresh_failure_count: Option<u32>,
    pub disabled_reason: CredentialRuntimeDisabledReasonPatch,
    pub warmup_remaining: Option<u32>,
    pub credential_disabled: Option<bool>,
    pub last_used_at: Option<String>,
    pub expected_generation: Option<u64>,
    pub advance_generation: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CredentialRuntimeFailureCounts {
    pub failure_count: Option<u32>,
    pub refresh_failure_count: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialRuntimeStateMutationResult {
    pub state: CredentialRuntimeStateRow,
    pub credential_disabled: bool,
    pub applied: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use = "credential runtime CAS conflicts must be handled explicitly"]
pub enum CredentialRuntimeStateCasOutcome {
    Applied(CredentialRuntimeStateRow),
    Conflict {
        current: Option<CredentialRuntimeStateRow>,
    },
}

#[derive(Debug, Clone, Default)]
pub struct CredentialAccountInfoRow {
    pub subscription_title: Option<String>,
    pub current_usage: f64,
    pub usage_limit: f64,
    pub remaining: f64,
    pub usage_percentage: f64,
    pub credit_limit: f64,
    pub credit_remaining: f64,
    pub credit_base: f64,
    pub credit_bonus: f64,
    pub overage_status: Option<String>,
    pub overage_capability: Option<String>,
    pub overage_cap: f64,
    pub overage_rate: f64,
    pub current_overages: f64,
    pub next_reset_at: Option<f64>,
    pub checked_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAuditLogRow {
    pub id: i64,
    pub created_at: String,
    pub actor: String,
    pub action: String,
    pub object_type: String,
    pub object_id: Option<String>,
    pub success: bool,
    pub error_message: Option<String>,
    pub detail: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAuditLogPage {
    pub page: usize,
    pub limit: usize,
    pub has_next: bool,
    pub records: Vec<AdminAuditLogRow>,
}

#[derive(Debug, Clone)]
pub struct PostgresUsageStore {
    store: Arc<PostgresStore>,
}

#[derive(Debug, Clone)]
pub struct UsageCleanupPreview {
    pub matched_rows: u64,
    pub oldest_created_at: Option<DateTime<Utc>>,
    pub newest_created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageCleanupBatchResult {
    pub processed_rows: u64,
    /// `None` means a full batch was processed and no completion probe was needed.
    /// A short or empty batch always carries an explicit existence-probe result.
    pub has_remaining: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct UsageCleanupJobRow {
    pub job_id: String,
    pub mode: String,
    pub cutoff_at: DateTime<Utc>,
    pub batch_size: usize,
    pub max_batches: usize,
    pub pause_ms_between_batches: u64,
    pub status: String,
    pub phase: String,
    pub matched_rows: Option<u64>,
    pub remaining_rows: Option<u64>,
    pub processed_rows: u64,
    pub last_batch_rows: u64,
    pub batches: usize,
    pub cancel_requested: bool,
    pub stop_reason: Option<String>,
    pub last_error: Option<String>,
    pub redis_deleted_keys: usize,
    pub redis_delete_commands: usize,
    pub redis_max_command_keys: usize,
    pub redis_scan_passes: usize,
    pub redis_used_del_fallback: bool,
    pub redis_pass_limit_reached: bool,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub lease_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewUsageCleanupJob<'a> {
    pub job_id: &'a str,
    pub mode: &'a str,
    pub cutoff_at: DateTime<Utc>,
    pub batch_size: usize,
    pub max_batches: usize,
    pub pause_ms_between_batches: u64,
}

#[derive(Debug, Clone)]
pub struct UsageCleanupJobProgress<'a> {
    pub job_id: &'a str,
    pub worker_id: &'a str,
    pub status: &'a str,
    pub phase: &'a str,
    pub processed_rows: u64,
    pub last_batch_rows: u64,
    pub batches: usize,
    pub remaining_rows: Option<u64>,
    pub stop_reason: Option<&'a str>,
    pub last_error: Option<&'a str>,
    pub redis_deleted_keys: usize,
    pub redis_delete_commands: usize,
    pub redis_max_command_keys: usize,
    pub redis_scan_passes: usize,
    pub redis_used_del_fallback: bool,
    pub redis_pass_limit_reached: bool,
    pub finished: bool,
}

impl PostgresUsageStore {
    pub fn new(store: Arc<PostgresStore>) -> Self {
        Self { store }
    }

    pub async fn create_cleanup_job(&self, job: NewUsageCleanupJob<'_>) -> anyhow::Result<bool> {
        let result = sqlx::query(
            r#"
            INSERT INTO usage_cleanup_jobs (
                job_id, mode, cutoff_at, batch_size, max_batches,
                pause_ms_between_batches, status, phase, started_at, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, 'queued', 'postgres', now(), now())
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(job.job_id)
        .bind(job.mode)
        .bind(job.cutoff_at)
        .bind(usize_to_i64(job.batch_size))
        .bind(usize_to_i64(job.max_batches))
        .bind(u64_to_i64(job.pause_ms_between_batches))
        .execute(self.store.pool())
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn latest_cleanup_job(&self) -> anyhow::Result<Option<UsageCleanupJobRow>> {
        let row = sqlx::query(
            r#"
            SELECT job_id, mode, cutoff_at, batch_size, max_batches,
                   pause_ms_between_batches, status, phase, matched_rows,
                   remaining_rows, processed_rows, last_batch_rows, batches,
                   cancel_requested, stop_reason, last_error, redis_deleted_keys,
                   redis_delete_commands, redis_max_command_keys, redis_scan_passes,
                   redis_used_del_fallback, redis_pass_limit_reached,
                   started_at, updated_at, finished_at,
                   lease_until
            FROM usage_cleanup_jobs
            ORDER BY started_at DESC, job_id DESC
            LIMIT 1
            "#,
        )
        .fetch_optional(self.store.pool())
        .await?;
        row.map(usage_cleanup_job_from_row).transpose()
    }

    pub async fn cleanup_job(&self, job_id: &str) -> anyhow::Result<Option<UsageCleanupJobRow>> {
        let row = sqlx::query(
            r#"
            SELECT job_id, mode, cutoff_at, batch_size, max_batches,
                   pause_ms_between_batches, status, phase, matched_rows,
                   remaining_rows, processed_rows, last_batch_rows, batches,
                   cancel_requested, stop_reason, last_error, redis_deleted_keys,
                   redis_delete_commands, redis_max_command_keys, redis_scan_passes,
                   redis_used_del_fallback, redis_pass_limit_reached,
                   started_at, updated_at, finished_at,
                   lease_until
            FROM usage_cleanup_jobs
            WHERE job_id = $1
            "#,
        )
        .bind(job_id)
        .fetch_optional(self.store.pool())
        .await?;
        row.map(usage_cleanup_job_from_row).transpose()
    }

    pub async fn recoverable_cleanup_job(&self) -> anyhow::Result<Option<UsageCleanupJobRow>> {
        let row = sqlx::query(
            r#"
            SELECT job_id, mode, cutoff_at, batch_size, max_batches,
                   pause_ms_between_batches, status, phase, matched_rows,
                   remaining_rows, processed_rows, last_batch_rows, batches,
                   cancel_requested, stop_reason, last_error, redis_deleted_keys,
                   redis_delete_commands, redis_max_command_keys, redis_scan_passes,
                   redis_used_del_fallback, redis_pass_limit_reached,
                   started_at, updated_at, finished_at,
                   lease_until
            FROM usage_cleanup_jobs
            WHERE status = 'queued'
               OR (
                    status = 'running'
                    AND (lease_until IS NULL OR lease_until < now())
               )
            ORDER BY started_at ASC, job_id ASC
            LIMIT 1
            "#,
        )
        .fetch_optional(self.store.pool())
        .await?;
        row.map(usage_cleanup_job_from_row).transpose()
    }

    pub async fn claim_cleanup_job(
        &self,
        job_id: &str,
        worker_id: &str,
        lease_secs: u64,
    ) -> anyhow::Result<Option<UsageCleanupJobRow>> {
        let row = sqlx::query(
            r#"
            UPDATE usage_cleanup_jobs
            SET status = 'running',
                lease_owner = $2,
                lease_until = now() + ($3::bigint * interval '1 second'),
                updated_at = now()
            WHERE job_id = $1
              AND (
                    status = 'queued'
                    OR (status = 'running' AND (lease_until IS NULL OR lease_until < now()))
              )
            RETURNING job_id, mode, cutoff_at, batch_size, max_batches,
                      pause_ms_between_batches, status, phase, matched_rows,
                      remaining_rows, processed_rows, last_batch_rows, batches,
                      cancel_requested, stop_reason, last_error, redis_deleted_keys,
                      redis_delete_commands, redis_max_command_keys, redis_scan_passes,
                      redis_used_del_fallback, redis_pass_limit_reached,
                      started_at, updated_at, finished_at,
                      lease_until
            "#,
        )
        .bind(job_id)
        .bind(worker_id)
        .bind(u64_to_i64(lease_secs.max(1)))
        .fetch_optional(self.store.pool())
        .await?;
        row.map(usage_cleanup_job_from_row).transpose()
    }

    pub async fn update_cleanup_job_progress(
        &self,
        progress: UsageCleanupJobProgress<'_>,
        lease_secs: u64,
    ) -> anyhow::Result<Option<bool>> {
        let row = sqlx::query(
            r#"
            UPDATE usage_cleanup_jobs
            SET status = $3,
                phase = $4,
                processed_rows = $5,
                last_batch_rows = $6,
                batches = $7,
                remaining_rows = $8,
                matched_rows = CASE
                    WHEN $17 AND $8::bigint = 0 THEN COALESCE(matched_rows, $5)
                    ELSE matched_rows
                END,
                stop_reason = $9,
                last_error = $10,
                redis_deleted_keys = $11,
                redis_delete_commands = $12,
                redis_max_command_keys = $13,
                redis_scan_passes = $14,
                redis_used_del_fallback = $15,
                redis_pass_limit_reached = $16,
                lease_until = CASE
                    WHEN $17 THEN NULL
                    ELSE now() + ($18::bigint * interval '1 second')
                END,
                lease_owner = CASE WHEN $17 THEN NULL ELSE lease_owner END,
                finished_at = CASE WHEN $17 THEN now() ELSE NULL END,
                updated_at = now()
            WHERE job_id = $1
              AND lease_owner = $2
            RETURNING cancel_requested
            "#,
        )
        .bind(progress.job_id)
        .bind(progress.worker_id)
        .bind(progress.status)
        .bind(progress.phase)
        .bind(u64_to_i64(progress.processed_rows))
        .bind(u64_to_i64(progress.last_batch_rows))
        .bind(usize_to_i64(progress.batches))
        .bind(progress.remaining_rows.map(u64_to_i64))
        .bind(progress.stop_reason)
        .bind(progress.last_error)
        .bind(usize_to_i64(progress.redis_deleted_keys))
        .bind(usize_to_i64(progress.redis_delete_commands))
        .bind(usize_to_i64(progress.redis_max_command_keys))
        .bind(usize_to_i64(progress.redis_scan_passes))
        .bind(progress.redis_used_del_fallback)
        .bind(progress.redis_pass_limit_reached)
        .bind(progress.finished)
        .bind(u64_to_i64(lease_secs.max(1)))
        .fetch_optional(self.store.pool())
        .await?;
        row.map(|row| row.try_get("cancel_requested"))
            .transpose()
            .map_err(Into::into)
    }

    pub async fn renew_cleanup_job_lease(
        &self,
        job_id: &str,
        worker_id: &str,
        lease_secs: u64,
    ) -> anyhow::Result<Option<bool>> {
        let row = sqlx::query(
            r#"
            UPDATE usage_cleanup_jobs
            SET lease_until = now() + ($3::bigint * interval '1 second'),
                updated_at = now()
            WHERE job_id = $1
              AND lease_owner = $2
              AND status = 'running'
            RETURNING cancel_requested
            "#,
        )
        .bind(job_id)
        .bind(worker_id)
        .bind(u64_to_i64(lease_secs.max(1)))
        .fetch_optional(self.store.pool())
        .await?;
        row.map(|row| row.try_get("cancel_requested"))
            .transpose()
            .map_err(Into::into)
    }

    pub async fn request_cleanup_cancel(
        &self,
        job_id: &str,
    ) -> anyhow::Result<Option<UsageCleanupJobRow>> {
        let row = sqlx::query(
            r#"
            UPDATE usage_cleanup_jobs
            SET cancel_requested = true,
                status = CASE WHEN status = 'queued' THEN 'cancelled' ELSE status END,
                stop_reason = CASE
                    WHEN status = 'queued' THEN 'cancel_requested_before_start'
                    ELSE stop_reason
                END,
                finished_at = CASE WHEN status = 'queued' THEN now() ELSE finished_at END,
                lease_owner = CASE WHEN status = 'queued' THEN NULL ELSE lease_owner END,
                lease_until = CASE WHEN status = 'queued' THEN NULL ELSE lease_until END,
                updated_at = now()
            WHERE job_id = $1
              AND status IN ('queued', 'running')
            RETURNING job_id, mode, cutoff_at, batch_size, max_batches,
                      pause_ms_between_batches, status, phase, matched_rows,
                      remaining_rows, processed_rows, last_batch_rows, batches,
                      cancel_requested, stop_reason, last_error, redis_deleted_keys,
                      redis_delete_commands, redis_max_command_keys, redis_scan_passes,
                      redis_used_del_fallback, redis_pass_limit_reached,
                      started_at, updated_at, finished_at,
                      lease_until
            "#,
        )
        .bind(job_id)
        .fetch_optional(self.store.pool())
        .await?;
        row.map(usage_cleanup_job_from_row).transpose()
    }

    pub async fn requeue_cleanup_job(
        &self,
        job_id: &str,
    ) -> anyhow::Result<Option<UsageCleanupJobRow>> {
        let row = sqlx::query(
            r#"
            UPDATE usage_cleanup_jobs
            SET status = 'queued',
                cancel_requested = false,
                stop_reason = NULL,
                last_error = NULL,
                redis_pass_limit_reached = false,
                finished_at = NULL,
                lease_owner = NULL,
                lease_until = NULL,
                updated_at = now()
            WHERE job_id = $1
              AND status IN ('failed', 'cancelled', 'paused')
            RETURNING job_id, mode, cutoff_at, batch_size, max_batches,
                      pause_ms_between_batches, status, phase, matched_rows,
                      remaining_rows, processed_rows, last_batch_rows, batches,
                      cancel_requested, stop_reason, last_error, redis_deleted_keys,
                      redis_delete_commands, redis_max_command_keys, redis_scan_passes,
                      redis_used_del_fallback, redis_pass_limit_reached,
                      started_at, updated_at, finished_at,
                      lease_until
            "#,
        )
        .bind(job_id)
        .fetch_optional(self.store.pool())
        .await?;
        row.map(usage_cleanup_job_from_row).transpose()
    }

    #[cfg(test)]
    pub async fn record(&self, record: UsageRecord) -> anyhow::Result<()> {
        self.record_batch(vec![record]).await
    }

    pub async fn record_batch(&self, records: Vec<UsageRecord>) -> anyhow::Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        let started_at = std::time::Instant::now();
        let input_count = records.len();
        let mut records_by_id: HashMap<String, UsageRecord> = HashMap::with_capacity(records.len());
        for record in records {
            records_by_id.insert(record.id.clone(), record);
        }
        let mut records: Vec<UsageRecord> = records_by_id.into_values().collect();
        records.sort_unstable_by(|left, right| left.id.cmp(&right.id));
        let ids: Vec<String> = records.iter().map(|record| record.id.clone()).collect();
        let mut tx = self.store.pool().begin().await?;
        acquire_usage_writer_commit_guard(&mut tx).await?;
        acquire_usage_record_commit_guards(&mut tx, &ids).await?;
        let old_rows = sqlx::query(
            r#"
            SELECT id, data
            FROM usage_records
            WHERE id = ANY($1)
              AND deleted_at IS NULL
              AND rollup_active
            ORDER BY id
            FOR UPDATE
            "#,
        )
        .bind(&ids)
        .fetch_all(&mut *tx)
        .await?;
        let mut old_records: HashMap<String, UsageRecord> = HashMap::with_capacity(old_rows.len());
        for row in old_rows {
            let id: String = row.try_get("id")?;
            let value: serde_json::Value = row.try_get("data")?;
            let mut record: UsageRecord = serde_json::from_value(value)?;
            apply_usage_record_legacy_cost_compatibility(&mut record);
            old_records.insert(id, record);
        }

        let mut rollups = UsageRollupBatchDelta::default();
        for record in &records {
            if upsert_usage_record_in_tx(&mut tx, record).await? {
                if let Some(old_record) = old_records.get(&record.id) {
                    rollups.add_record(old_record, -1);
                }
                rollups.add_record(record, 1);
            }
        }
        rollups.apply(&mut tx).await?;
        tx.commit().await?;
        let elapsed = started_at.elapsed();
        if elapsed >= std::time::Duration::from_millis(500)
            || (records.len() > 1 && elapsed >= std::time::Duration::from_millis(250))
        {
            tracing::warn!(
                input_count,
                persisted_count = records.len(),
                elapsed_ms = elapsed.as_millis() as u64,
                "PgSQL usage 批量写入耗时较长"
            );
        } else if elapsed >= std::time::Duration::from_millis(100) {
            tracing::debug!(
                input_count,
                persisted_count = records.len(),
                elapsed_ms = elapsed.as_millis() as u64,
                "PgSQL usage 批量写入耗时略高"
            );
        }
        Ok(())
    }

    pub async fn query(&self, query: UsageRecordQuery) -> anyhow::Result<UsageRecordsResult> {
        let limit = normalize_query_limit(query.limit);
        let records = self
            .load_matching(query.clone(), 0, limit.saturating_add(1))
            .await?;
        let total = self.count_matching(query).await?;
        let mut records = records;
        records.truncate(limit);
        Ok(UsageRecordsResult { total, records })
    }

    pub async fn query_page(
        &self,
        query: UsageRecordQuery,
        page: usize,
        limit: usize,
    ) -> anyhow::Result<UsageRecordsPageResult> {
        let page = page.max(1);
        let limit = normalize_page_limit(limit);
        let offset = page.saturating_sub(1).saturating_mul(limit);
        let mut records = self
            .load_matching(query, offset, limit.saturating_add(1))
            .await?;
        let has_next = records.len() > limit;
        if has_next {
            records.truncate(limit);
        }
        Ok(UsageRecordsPageResult {
            page,
            limit,
            has_next,
            records,
        })
    }

    pub async fn preview_soft_delete_cleanup(
        &self,
        cutoff: DateTime<Utc>,
    ) -> anyhow::Result<UsageCleanupPreview> {
        let row = sqlx::query(
            r#"
            SELECT
                COUNT(*)::bigint AS matched_rows,
                MIN(created_at) AS oldest_created_at,
                MAX(created_at) AS newest_created_at
            FROM usage_records
            WHERE deleted_at IS NULL
              AND created_at < $1
            "#,
        )
        .bind(cutoff)
        .fetch_one(self.store.pool())
        .await?;
        cleanup_preview_from_row(row)
    }

    pub async fn preview_hard_delete_cleanup(
        &self,
        cutoff: DateTime<Utc>,
    ) -> anyhow::Result<UsageCleanupPreview> {
        let row = sqlx::query(
            r#"
            SELECT
                COUNT(*)::bigint AS matched_rows,
                MIN(created_at) AS oldest_created_at,
                MAX(created_at) AS newest_created_at
            FROM usage_records
            WHERE deleted_at IS NOT NULL
              AND deleted_at < $1
            "#,
        )
        .bind(cutoff)
        .fetch_one(self.store.pool())
        .await?;
        cleanup_preview_from_row(row)
    }

    pub async fn advance_soft_delete_cleanup_watermark(
        &self,
        cutoff: DateTime<Utc>,
    ) -> anyhow::Result<DateTime<Utc>> {
        let mut tx = self.store.pool().begin().await?;
        configure_usage_cleanup_transaction(&mut tx).await?;
        acquire_usage_cleanup_commit_guard(&mut tx).await?;
        let effective_cutoff = sqlx::query_scalar(
            r#"
            WITH advanced AS (
                INSERT INTO usage_cleanup_watermarks (scope, cutoff_at, updated_at)
                VALUES ($1, $2, now())
                ON CONFLICT (scope) DO UPDATE
                SET cutoff_at = EXCLUDED.cutoff_at,
                    updated_at = now()
                WHERE EXCLUDED.cutoff_at > usage_cleanup_watermarks.cutoff_at
                RETURNING cutoff_at
            )
            SELECT COALESCE(
                (SELECT cutoff_at FROM advanced),
                (SELECT cutoff_at FROM usage_cleanup_watermarks WHERE scope = $1)
            )
            "#,
        )
        .bind(USAGE_SOFT_DELETE_WATERMARK_SCOPE)
        .bind(cutoff)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(effective_cutoff)
    }

    pub async fn soft_delete_cleanup_watermark(&self) -> anyhow::Result<Option<DateTime<Utc>>> {
        sqlx::query_scalar("SELECT cutoff_at FROM usage_cleanup_watermarks WHERE scope = $1")
            .bind(USAGE_SOFT_DELETE_WATERMARK_SCOPE)
            .fetch_optional(self.store.pool())
            .await
            .map_err(Into::into)
    }

    pub async fn soft_delete_cleanup_batch(
        &self,
        cutoff: DateTime<Utc>,
        batch_size: usize,
    ) -> anyhow::Result<UsageCleanupBatchResult> {
        if let Err(err) = self.advance_soft_delete_cleanup_watermark(cutoff).await {
            if is_postgres_cleanup_retryable_timeout_error(&err) {
                return Ok(UsageCleanupBatchResult {
                    processed_rows: 0,
                    has_remaining: Some(true),
                });
            }
            return Err(err);
        }
        let mut tx = self.store.pool().begin().await?;
        configure_usage_cleanup_transaction(&mut tx).await?;
        if !try_acquire_usage_cleanup_commit_guard(&mut tx).await? {
            tx.rollback().await?;
            return Ok(UsageCleanupBatchResult {
                processed_rows: 0,
                has_remaining: Some(true),
            });
        }
        let batch_result = async {
            let victim_rows = sqlx::query(
                r#"
                WITH victims AS (
                    SELECT id, data, rollup_active
                    FROM usage_records
                    WHERE deleted_at IS NULL
                      AND created_at < $1
                    ORDER BY created_at ASC, id ASC
                    LIMIT $2
                    FOR UPDATE SKIP LOCKED
                )
                UPDATE usage_records AS u
                SET deleted_at = now(), rollup_active = false, updated_at = now()
                FROM victims AS v
                WHERE u.id = v.id
                RETURNING v.data, v.rollup_active
                "#,
            )
            .bind(cutoff)
            .bind(usize_to_i64(batch_size))
            .fetch_all(&mut *tx)
            .await?;
            let processed_rows = victim_rows.len() as u64;
            let mut rollups = UsageRollupBatchDelta::default();
            for row in victim_rows {
                if row.try_get::<bool, _>("rollup_active")? {
                    let value: serde_json::Value = row.try_get("data")?;
                    let mut record: UsageRecord = serde_json::from_value(value)?;
                    apply_usage_record_legacy_cost_compatibility(&mut record);
                    rollups.add_record(&record, -1);
                }
            }
            rollups.apply(&mut tx).await?;
            let has_remaining = if processed_rows < batch_size as u64 {
                Some(
                    sqlx::query_scalar::<_, bool>(
                        r#"
                        SELECT EXISTS (
                            SELECT 1
                            FROM usage_records
                            WHERE deleted_at IS NULL
                              AND created_at < $1
                        )
                        "#,
                    )
                    .bind(cutoff)
                    .fetch_one(&mut *tx)
                    .await?,
                )
            } else {
                None
            };
            anyhow::Ok(UsageCleanupBatchResult {
                processed_rows,
                has_remaining,
            })
        }
        .await;
        let batch_result = match batch_result {
            Ok(result) => result,
            Err(err) => {
                if is_postgres_cleanup_retryable_timeout_error(&err) {
                    tx.rollback().await?;
                    return Ok(UsageCleanupBatchResult {
                        processed_rows: 0,
                        has_remaining: Some(true),
                    });
                }
                return Err(err);
            }
        };
        tx.commit().await?;
        Ok(batch_result)
    }

    pub async fn soft_delete_cleanup_has_remaining(
        &self,
        cutoff: DateTime<Utc>,
    ) -> anyhow::Result<bool> {
        sqlx::query_scalar(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM usage_records
                WHERE deleted_at IS NULL
                  AND created_at < $1
            )
            "#,
        )
        .bind(cutoff)
        .fetch_one(self.store.pool())
        .await
        .map_err(Into::into)
    }

    pub async fn hard_delete_cleanup_batch(
        &self,
        cutoff: DateTime<Utc>,
        batch_size: usize,
    ) -> anyhow::Result<UsageCleanupBatchResult> {
        let mut tx = self.store.pool().begin().await?;
        configure_usage_cleanup_transaction(&mut tx).await?;
        if !try_acquire_usage_cleanup_commit_guard(&mut tx).await? {
            tx.rollback().await?;
            return Ok(UsageCleanupBatchResult {
                processed_rows: 0,
                has_remaining: Some(true),
            });
        }
        let batch_result = async {
            let victim_rows = sqlx::query(
                r#"
                WITH victims AS (
                    SELECT id, data, rollup_active
                    FROM usage_records
                    WHERE deleted_at IS NOT NULL
                      AND deleted_at < $1
                    ORDER BY deleted_at ASC, id ASC
                    LIMIT $2
                    FOR UPDATE SKIP LOCKED
                )
                DELETE FROM usage_records AS u
                USING victims AS v
                WHERE u.id = v.id
                RETURNING v.data, v.rollup_active
                "#,
            )
            .bind(cutoff)
            .bind(usize_to_i64(batch_size))
            .fetch_all(&mut *tx)
            .await?;
            let processed_rows = victim_rows.len() as u64;
            let mut rollups = UsageRollupBatchDelta::default();
            for row in victim_rows {
                if row.try_get::<bool, _>("rollup_active")? {
                    let value: serde_json::Value = row.try_get("data")?;
                    let mut record: UsageRecord = serde_json::from_value(value)?;
                    apply_usage_record_legacy_cost_compatibility(&mut record);
                    rollups.add_record(&record, -1);
                }
            }
            rollups.apply(&mut tx).await?;
            let has_remaining = if processed_rows < batch_size as u64 {
                Some(
                    sqlx::query_scalar::<_, bool>(
                        r#"
                        SELECT EXISTS (
                            SELECT 1
                            FROM usage_records
                            WHERE deleted_at IS NOT NULL
                              AND deleted_at < $1
                        )
                        "#,
                    )
                    .bind(cutoff)
                    .fetch_one(&mut *tx)
                    .await?,
                )
            } else {
                None
            };
            anyhow::Ok(UsageCleanupBatchResult {
                processed_rows,
                has_remaining,
            })
        }
        .await;
        let batch_result = match batch_result {
            Ok(result) => result,
            Err(err) => {
                if is_postgres_cleanup_retryable_timeout_error(&err) {
                    tx.rollback().await?;
                    return Ok(UsageCleanupBatchResult {
                        processed_rows: 0,
                        has_remaining: Some(true),
                    });
                }
                return Err(err);
            }
        };
        tx.commit().await?;
        Ok(batch_result)
    }

    pub async fn hard_delete_cleanup_has_remaining(
        &self,
        cutoff: DateTime<Utc>,
    ) -> anyhow::Result<bool> {
        sqlx::query_scalar(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM usage_records
                WHERE deleted_at IS NOT NULL
                  AND deleted_at < $1
            )
            "#,
        )
        .bind(cutoff)
        .fetch_one(self.store.pool())
        .await
        .map_err(Into::into)
    }

    pub async fn summary(&self, high_cache_threshold: i32) -> anyhow::Result<UsageSummary> {
        let row = sqlx::query(
            r#"
            SELECT
                COALESCE(t.requests, 0)::bigint AS total_requests,
                COALESCE(t.success_requests, 0)::bigint AS success_requests,
                COALESCE(t.error_requests, 0)::bigint AS error_requests,
                COALESCE((
                    SELECT SUM(requests)
                    FROM usage_cache_read_totals
                    WHERE cache_read_input_tokens >= $1
                ), 0)::bigint AS high_cache_requests,
                COALESCE(t.total_input_tokens, 0)::bigint AS total_input_tokens,
                COALESCE(t.total_output_tokens, 0)::bigint AS total_output_tokens,
                COALESCE(t.total_cache_read_input_tokens, 0)::bigint AS total_cache_read_input_tokens,
                COALESCE(t.total_cache_creation_input_tokens, 0)::bigint AS total_cache_creation_input_tokens,
                COALESCE(t.total_estimated_cost_usd, 0)::double precision AS total_estimated_cost_usd,
                CASE
                    WHEN COALESCE(t.total_original_cost_usd, 0) <> 0
                    THEN t.total_original_cost_usd
                    ELSE COALESCE(t.total_estimated_cost_usd, 0)
                END::double precision AS total_original_cost_usd,
                COALESCE(t.total_kiro_metering_usage, 0)::double precision
                    AS total_kiro_metering_usage,
                COALESCE(t.priced_requests, 0)::bigint AS priced_requests,
                COALESCE(t.unpriced_requests, 0)::bigint AS unpriced_requests,
                COALESCE(t.local_prompt_cache_requests, 0)::bigint AS local_prompt_cache_requests,
                COALESCE(t.local_prompt_cache_input_tokens, 0)::bigint AS local_prompt_cache_input_tokens,
                COALESCE(t.local_prompt_cache_read_input_tokens, 0)::bigint AS local_prompt_cache_read_input_tokens,
                COALESCE(t.local_prompt_cache_creation_input_tokens, 0)::bigint AS local_prompt_cache_creation_input_tokens,
                COALESCE(t.simulated_requests, 0)::bigint AS simulated_requests,
                COALESCE(t.upstream_metadata_requests, 0)::bigint AS upstream_metadata_requests,
                COALESCE(t.external_pool_requests, 0)::bigint AS external_pool_requests,
                COALESCE(t.external_pool_priced_requests, 0)::bigint AS external_pool_priced_requests,
                COALESCE(t.external_pool_unpriced_requests, 0)::bigint AS external_pool_unpriced_requests,
                COALESCE(t.external_pool_cost_floor_applied_requests, 0)::bigint AS external_pool_cost_floor_applied_requests,
                COALESCE(t.external_pool_raw_cost_usd, 0)::double precision AS external_pool_raw_cost_usd,
                CASE
                    WHEN COALESCE(t.external_pool_shaped_cost_usd, 0) <> 0
                    THEN t.external_pool_shaped_cost_usd
                    ELSE COALESCE(t.external_pool_reported_cost_usd, 0)
                END::double precision AS external_pool_shaped_cost_usd,
                CASE
                    WHEN COALESCE(t.external_pool_uplifted_cost_usd, 0) <> 0
                    THEN t.external_pool_uplifted_cost_usd
                    ELSE COALESCE(t.external_pool_reported_cost_usd, 0)
                END::double precision AS external_pool_uplifted_cost_usd,
                CASE
                    WHEN COALESCE(t.external_pool_profit_usd, 0) <> 0
                    THEN t.external_pool_profit_usd
                    WHEN COALESCE(t.external_pool_reported_cost_usd, 0) <> 0
                      OR COALESCE(t.external_pool_raw_cost_usd, 0) <> 0
                    THEN COALESCE(t.external_pool_reported_cost_usd, 0) - COALESCE(t.external_pool_raw_cost_usd, 0)
                    ELSE 0
                END::double precision AS external_pool_profit_usd,
                COALESCE(t.external_pool_reported_cost_usd, 0)::double precision AS external_pool_reported_cost_usd,
                COALESCE(t.external_pool_billable_cost_usd, 0)::double precision AS external_pool_billable_cost_usd,
                COALESCE(t.external_pool_cost_floor_delta_usd, 0)::double precision AS external_pool_cost_floor_delta_usd,
                COUNT(r.id) FILTER (WHERE r.created_at >= now() - interval '60 seconds')::bigint AS realtime_requests,
                COUNT(r.id) FILTER (WHERE r.created_at >= now() - interval '60 seconds' AND r.status = 'success')::bigint AS realtime_success_requests,
                COUNT(r.id) FILTER (WHERE r.created_at >= now() - interval '60 seconds' AND r.status <> 'success')::bigint AS realtime_error_requests,
                COALESCE(SUM(r.total_input_tokens) FILTER (WHERE r.created_at >= now() - interval '60 seconds'), 0)::bigint AS realtime_input_tokens,
                COALESCE(SUM(r.output_tokens) FILTER (WHERE r.created_at >= now() - interval '60 seconds'), 0)::bigint AS realtime_output_tokens,
                COALESCE(SUM(r.billable_input_tokens) FILTER (WHERE r.created_at >= now() - interval '60 seconds'), 0)::bigint AS realtime_billable_input_tokens
            FROM (SELECT 1) AS anchor
            LEFT JOIN usage_rollup_totals t
              ON t.dimension = 'global'
             AND t.dimension_key = 'all'
            LEFT JOIN usage_records r
              ON r.deleted_at IS NULL
             AND r.created_at >= now() - interval '60 seconds'
            GROUP BY t.requests, t.success_requests, t.error_requests,
                     t.total_input_tokens, t.total_output_tokens,
                     t.total_cache_read_input_tokens, t.total_cache_creation_input_tokens,
                     t.total_estimated_cost_usd, t.total_original_cost_usd,
                     t.total_kiro_metering_usage, t.priced_requests, t.unpriced_requests,
                     t.local_prompt_cache_requests, t.local_prompt_cache_input_tokens,
                     t.local_prompt_cache_read_input_tokens,
                     t.local_prompt_cache_creation_input_tokens,
                     t.simulated_requests, t.upstream_metadata_requests,
                     t.external_pool_requests, t.external_pool_priced_requests,
                     t.external_pool_unpriced_requests,
                     t.external_pool_cost_floor_applied_requests,
                     t.external_pool_raw_cost_usd, t.external_pool_shaped_cost_usd,
                     t.external_pool_uplifted_cost_usd, t.external_pool_profit_usd,
                     t.external_pool_reported_cost_usd, t.external_pool_billable_cost_usd,
                     t.external_pool_cost_floor_delta_usd
            "#,
        )
        .bind(high_cache_threshold)
        .fetch_one(self.store.pool())
        .await?;

        Ok(UsageSummary {
            total_requests: row_i64_to_usize(&row, "total_requests")?,
            success_requests: row_i64_to_usize(&row, "success_requests")?,
            error_requests: row_i64_to_usize(&row, "error_requests")?,
            high_cache_requests: row_i64_to_usize(&row, "high_cache_requests")?,
            total_input_tokens: row.try_get("total_input_tokens")?,
            total_output_tokens: row.try_get("total_output_tokens")?,
            total_cache_read_input_tokens: row.try_get("total_cache_read_input_tokens")?,
            total_cache_creation_input_tokens: row.try_get("total_cache_creation_input_tokens")?,
            total_estimated_cost_usd: row.try_get("total_estimated_cost_usd")?,
            total_original_cost_usd: row.try_get("total_original_cost_usd")?,
            total_kiro_metering_usage: row.try_get("total_kiro_metering_usage")?,
            priced_requests: row_i64_to_usize(&row, "priced_requests")?,
            unpriced_requests: row_i64_to_usize(&row, "unpriced_requests")?,
            local_prompt_cache_requests: row_i64_to_usize(&row, "local_prompt_cache_requests")?,
            local_prompt_cache_input_tokens: row.try_get("local_prompt_cache_input_tokens")?,
            local_prompt_cache_read_input_tokens: row
                .try_get("local_prompt_cache_read_input_tokens")?,
            local_prompt_cache_creation_input_tokens: row
                .try_get("local_prompt_cache_creation_input_tokens")?,
            simulated_requests: row_i64_to_usize(&row, "simulated_requests")?,
            upstream_metadata_requests: row_i64_to_usize(&row, "upstream_metadata_requests")?,
            external_pool_billing: UsageExternalPoolBillingSummary {
                requests: row_i64_to_usize(&row, "external_pool_requests")?,
                priced_requests: row_i64_to_usize(&row, "external_pool_priced_requests")?,
                unpriced_requests: row_i64_to_usize(&row, "external_pool_unpriced_requests")?,
                cost_floor_applied_requests: row_i64_to_usize(
                    &row,
                    "external_pool_cost_floor_applied_requests",
                )?,
                raw_cost_usd: row.try_get("external_pool_raw_cost_usd")?,
                shaped_cost_usd: row.try_get("external_pool_shaped_cost_usd")?,
                uplifted_cost_usd: row.try_get("external_pool_uplifted_cost_usd")?,
                profit_usd: row.try_get("external_pool_profit_usd")?,
                reported_cost_usd: row.try_get("external_pool_reported_cost_usd")?,
                billable_cost_usd: row.try_get("external_pool_billable_cost_usd")?,
                cost_floor_delta_usd: row.try_get("external_pool_cost_floor_delta_usd")?,
            },
            realtime: UsageRealtimeStats::from_totals_with_status(
                REALTIME_USAGE_WINDOW_SECS,
                row_i64_to_usize(&row, "realtime_requests")?,
                row_i64_to_usize(&row, "realtime_success_requests")?,
                row_i64_to_usize(&row, "realtime_error_requests")?,
                row.try_get("realtime_input_tokens")?,
                row.try_get("realtime_output_tokens")?,
                row.try_get("realtime_billable_input_tokens")?,
            ),
            top_credentials: self.top_credential_aggregates().await?,
            top_conversations: self.top_conversation_aggregates().await?,
        })
    }

    pub async fn dashboard(
        &self,
        timezone: Option<&str>,
        high_cache_threshold: i32,
    ) -> anyhow::Result<UsageDashboardResponse> {
        let now = Utc::now();
        let generated_at = now.to_rfc3339();
        let (timezone, offset) = usage_dashboard_timezone(timezone);
        let window_specs = usage_dashboard_windows(now, offset);
        let mut windows = self
            .dashboard_windows_with_basic_fallback(&window_specs, high_cache_threshold)
            .await?;
        if let Err(err) = self
            .populate_dashboard_window_details(&window_specs, &mut windows)
            .await
        {
            tracing::warn!(
                error = %err,
                "usage dashboard 窗口明细聚合失败，保留基础窗口数据"
            );
        }
        let (_, _, series) = self.dashboard_series_only(Some(&timezone)).await?;
        let (_, top) = self.dashboard_top_only().await?;
        Ok(UsageDashboardResponse {
            generated_at,
            timezone,
            windows,
            series,
            top,
        })
    }

    pub async fn dashboard_windows_only(
        &self,
        timezone: Option<&str>,
        high_cache_threshold: i32,
    ) -> anyhow::Result<(String, String, Vec<UsageDashboardWindow>)> {
        let now = Utc::now();
        let (timezone, offset) = usage_dashboard_timezone(timezone);
        let window_specs = usage_dashboard_windows(now, offset);
        let windows = self
            .dashboard_windows_with_basic_fallback(&window_specs, high_cache_threshold)
            .await?;
        Ok((now.to_rfc3339(), timezone, windows))
    }

    pub async fn dashboard_series_only(
        &self,
        timezone: Option<&str>,
    ) -> anyhow::Result<(String, String, UsageDashboardSeries)> {
        let now = Utc::now();
        let (timezone, offset) = usage_dashboard_timezone(timezone);
        let hourly_specs = usage_dashboard_hourly_windows(now, offset);
        let daily_specs = usage_dashboard_daily_windows(now, offset);
        Ok((
            now.to_rfc3339(),
            timezone,
            UsageDashboardSeries {
                hourly_24h: self.dashboard_series(&hourly_specs).await?,
                daily_7d: self.dashboard_series(&daily_specs).await?,
            },
        ))
    }

    pub async fn dashboard_top_only(&self) -> anyhow::Result<(String, UsageDashboardTop)> {
        let now = Utc::now();
        let models = self
            .dashboard_top_aggregates(DashboardTopGroup::Model)
            .await?;
        let credentials = self
            .dashboard_top_aggregates(DashboardTopGroup::Credential)
            .await?;
        let endpoints = self
            .dashboard_top_aggregates(DashboardTopGroup::Endpoint)
            .await?;
        let errors = self
            .dashboard_top_aggregates(DashboardTopGroup::Error)
            .await?;
        let models_len = models.items.len();
        let credentials_len = credentials.items.len();
        let endpoints_len = endpoints.items.len();
        let errors_len = errors.items.len();
        Ok((
            now.to_rfc3339(),
            UsageDashboardTop {
                window_key: "lifetime".to_string(),
                models: models.items,
                credentials: credentials.items,
                endpoints: endpoints.items,
                errors: errors.items,
                models_total: models.total,
                credentials_total: credentials.total,
                endpoints_total: endpoints.total,
                errors_total: errors.total,
                models_truncated: models.total > models_len,
                credentials_truncated: credentials.total > credentials_len,
                endpoints_truncated: endpoints.total > endpoints_len,
                errors_truncated: errors.total > errors_len,
                order_by: "estimated_cost_usd".to_string(),
                errors_order_by: "error_requests".to_string(),
            },
        ))
    }

    pub async fn dashboard_top_for_window(
        &self,
        timezone: Option<&str>,
        window_key: &str,
    ) -> anyhow::Result<(String, UsageDashboardTop)> {
        let now = Utc::now();
        let (_timezone, offset) = usage_dashboard_timezone(timezone);
        let window_spec = usage_dashboard_window_spec_for_key(now, offset, window_key);
        let specs = [window_spec.clone()];
        let models = self
            .dashboard_top_aggregates_for_window(&specs, DashboardTopGroup::Model)
            .await?;
        let credentials = self
            .dashboard_top_aggregates_for_window(&specs, DashboardTopGroup::Credential)
            .await?;
        let endpoints = self
            .dashboard_top_aggregates_for_window(&specs, DashboardTopGroup::Endpoint)
            .await?;
        let errors = self
            .dashboard_top_aggregates_for_window(&specs, DashboardTopGroup::Error)
            .await?;
        let models_len = models.items.len();
        let credentials_len = credentials.items.len();
        let endpoints_len = endpoints.items.len();
        let errors_len = errors.items.len();
        Ok((
            now.to_rfc3339(),
            UsageDashboardTop {
                window_key: window_spec.key,
                models: models.items,
                credentials: credentials.items,
                endpoints: endpoints.items,
                errors: errors.items,
                models_total: models.total,
                credentials_total: credentials.total,
                endpoints_total: endpoints.total,
                errors_total: errors.total,
                models_truncated: models.total > models_len,
                credentials_truncated: credentials.total > credentials_len,
                endpoints_truncated: endpoints.total > endpoints_len,
                errors_truncated: errors.total > errors_len,
                order_by: "estimated_cost_usd".to_string(),
                errors_order_by: "error_requests".to_string(),
            },
        ))
    }

    /// Read all credential usage aggregates for one dashboard window.
    ///
    /// The query is rollup-first and only scans the two partial boundary hours
    /// from `usage_records`, matching the existing dashboard aggregation rules.
    /// It is read-only and never touches credential runtime state.
    pub async fn dashboard_credential_aggregates_for_window(
        &self,
        timezone: Option<&str>,
        window_key: &str,
        credential_ids: &[u64],
    ) -> anyhow::Result<(
        String,
        String,
        String,
        Vec<UsageDashboardCredentialAggregate>,
    )> {
        let now = Utc::now();
        let (timezone, offset) = usage_dashboard_timezone(timezone);
        let window_spec = usage_dashboard_window_spec_for_key(now, offset, window_key);
        let specs = [window_spec.clone()];
        if credential_ids.is_empty() {
            return Ok((now.to_rfc3339(), timezone, window_spec.key, Vec::new()));
        }
        let credential_keys: Vec<String> = credential_ids.iter().map(|id| id.to_string()).collect();
        let credential_ids_i64: Vec<i64> = credential_ids
            .iter()
            .filter_map(|id| i64::try_from(*id).ok())
            .collect();

        let mut builder = QueryBuilder::<Postgres>::new("");
        push_dashboard_windows_cte(&mut builder, &specs);
        builder.push(
            r#", window_credential_segments AS (
            SELECT
                b.dimension_key AS credential_key,
                b.requests,
                b.error_requests,
                b.total_input_tokens,
                b.total_output_tokens,
                b.total_estimated_cost_usd,
                CASE
                    WHEN COALESCE(b.total_original_cost_usd, 0) <> 0
                    THEN b.total_original_cost_usd
                    ELSE COALESCE(b.total_estimated_cost_usd, 0)
                END::double precision AS total_original_cost_usd,
                b.total_kiro_metering_usage,
                b.priced_requests,
                b.unpriced_requests
            FROM window_bounds w
            JOIN usage_rollup_time_buckets b
              ON b.dimension = 'credential'
             AND b.dimension_key = ANY(
            "#,
        );
        builder.push_bind(&credential_keys);
        builder.push(
            r#"::text[])
             AND b.bucket_start >= w.full_from_at
             AND b.bucket_start < w.full_to_at
            UNION ALL
            SELECT
                r.credential_id::text AS credential_key,
                COUNT(*)::bigint AS requests,
                COUNT(*) FILTER (WHERE r.status <> 'success')::bigint AS error_requests,
                COALESCE(SUM(r.total_input_tokens), 0)::bigint AS total_input_tokens,
                COALESCE(SUM(r.output_tokens), 0)::bigint AS total_output_tokens,
                COALESCE(SUM(r.estimated_cost_usd), 0)::double precision
                    AS total_estimated_cost_usd,
                COALESCE(SUM(
                    CASE
                        WHEN COALESCE(r.original_cost_usd, 0) <> 0
                        THEN r.original_cost_usd
                        ELSE COALESCE(r.estimated_cost_usd, 0)
                    END
                ), 0)::double precision AS total_original_cost_usd,
                COALESCE(SUM(r.kiro_metering_usage), 0)::double precision
                    AS total_kiro_metering_usage,
                COUNT(*) FILTER (WHERE r.pricing_available)::bigint AS priced_requests,
                COUNT(*) FILTER (WHERE NOT r.pricing_available)::bigint AS unpriced_requests
            FROM window_boundary_records r
            WHERE r.credential_id IS NOT NULL
              AND r.credential_id = ANY(
            "#,
        );
        builder.push_bind(&credential_ids_i64);
        builder.push(
            r#"::bigint[])
            GROUP BY r.credential_id
            ), window_credential_totals AS (
            SELECT
                credential_key,
                SUM(requests)::bigint AS requests,
                SUM(error_requests)::bigint AS error_requests,
                SUM(total_input_tokens)::bigint AS total_input_tokens,
                SUM(total_output_tokens)::bigint AS total_output_tokens,
                SUM(total_estimated_cost_usd)::double precision AS total_estimated_cost_usd,
                SUM(total_original_cost_usd)::double precision AS total_original_cost_usd,
                SUM(total_kiro_metering_usage)::double precision AS total_kiro_metering_usage,
                SUM(priced_requests)::bigint AS priced_requests,
                SUM(unpriced_requests)::bigint AS unpriced_requests
            FROM window_credential_segments
            GROUP BY credential_key
            ), lifetime_credentials AS (
            SELECT
                dimension_key,
                requests,
                error_requests,
                total_input_tokens,
                total_output_tokens,
                total_estimated_cost_usd,
                total_original_cost_usd,
                total_kiro_metering_usage,
                priced_requests,
                unpriced_requests
            FROM usage_rollup_totals
            WHERE dimension = 'credential'
              AND dimension_key = ANY(
            "#,
        );
        builder.push_bind(&credential_keys);
        builder.push(
            r#"::text[])
            )
            SELECT
                COALESCE(window_credential_totals.credential_key, lifetime.dimension_key)
                    AS credential_id,
                COALESCE(window_credential_totals.requests, 0)::bigint AS window_requests,
                COALESCE(window_credential_totals.error_requests, 0)::bigint
                    AS window_error_requests,
                COALESCE(window_credential_totals.total_input_tokens, 0)::bigint
                    AS window_total_input_tokens,
                COALESCE(window_credential_totals.total_output_tokens, 0)::bigint
                    AS window_total_output_tokens,
                COALESCE(window_credential_totals.total_estimated_cost_usd, 0)::double precision
                    AS window_estimated_cost_usd,
                COALESCE(window_credential_totals.total_original_cost_usd, 0)::double precision
                    AS window_original_cost_usd,
                COALESCE(window_credential_totals.total_kiro_metering_usage, 0)::double precision
                    AS window_kiro_metering_usage,
                COALESCE(window_credential_totals.priced_requests, 0)::bigint
                    AS window_priced_requests,
                COALESCE(window_credential_totals.unpriced_requests, 0)::bigint
                    AS window_unpriced_requests,
                COALESCE(lifetime.requests, 0)::bigint AS lifetime_requests,
                COALESCE(lifetime.error_requests, 0)::bigint AS lifetime_error_requests,
                COALESCE(lifetime.total_input_tokens, 0)::bigint
                    AS lifetime_total_input_tokens,
                COALESCE(lifetime.total_output_tokens, 0)::bigint
                    AS lifetime_total_output_tokens,
                COALESCE(lifetime.total_estimated_cost_usd, 0)::double precision
                    AS lifetime_estimated_cost_usd,
                COALESCE(
                    CASE
                        WHEN COALESCE(lifetime.total_original_cost_usd, 0) <> 0
                        THEN lifetime.total_original_cost_usd
                        ELSE lifetime.total_estimated_cost_usd
                    END,
                    0
                )::double precision AS lifetime_original_cost_usd,
                COALESCE(lifetime.total_kiro_metering_usage, 0)::double precision
                    AS lifetime_kiro_metering_usage,
                COALESCE(lifetime.priced_requests, 0)::bigint AS lifetime_priced_requests,
                COALESCE(lifetime.unpriced_requests, 0)::bigint AS lifetime_unpriced_requests
            FROM lifetime_credentials lifetime
            FULL OUTER JOIN window_credential_totals
              ON window_credential_totals.credential_key = lifetime.dimension_key
            WHERE window_credential_totals.credential_key IS NOT NULL
               OR lifetime.dimension_key IS NOT NULL
            ORDER BY credential_id
            "#,
        );

        let mut tx = self.store.pool().begin().await?;
        configure_usage_dashboard_read_transaction(&mut tx).await?;
        let rows = builder.build().fetch_all(&mut *tx).await?;
        tx.commit().await?;

        let mut aggregates = Vec::with_capacity(rows.len());
        for row in rows {
            let credential_id: String = row.try_get("credential_id")?;
            let Ok(credential_id) = credential_id.parse::<u64>() else {
                continue;
            };
            aggregates.push(UsageDashboardCredentialAggregate {
                credential_id,
                window_requests: row_i64_to_usize(&row, "window_requests")?,
                window_error_requests: row_i64_to_usize(&row, "window_error_requests")?,
                window_total_input_tokens: row.try_get("window_total_input_tokens")?,
                window_total_output_tokens: row.try_get("window_total_output_tokens")?,
                window_estimated_cost_usd: row.try_get("window_estimated_cost_usd")?,
                window_original_cost_usd: row.try_get("window_original_cost_usd")?,
                window_kiro_metering_usage: row.try_get("window_kiro_metering_usage")?,
                window_priced_requests: row_i64_to_usize(&row, "window_priced_requests")?,
                window_unpriced_requests: row_i64_to_usize(&row, "window_unpriced_requests")?,
                lifetime_requests: row_i64_to_usize(&row, "lifetime_requests")?,
                lifetime_error_requests: row_i64_to_usize(&row, "lifetime_error_requests")?,
                lifetime_total_input_tokens: row.try_get("lifetime_total_input_tokens")?,
                lifetime_total_output_tokens: row.try_get("lifetime_total_output_tokens")?,
                lifetime_estimated_cost_usd: row.try_get("lifetime_estimated_cost_usd")?,
                lifetime_original_cost_usd: row.try_get("lifetime_original_cost_usd")?,
                lifetime_kiro_metering_usage: row.try_get("lifetime_kiro_metering_usage")?,
                lifetime_priced_requests: row_i64_to_usize(&row, "lifetime_priced_requests")?,
                lifetime_unpriced_requests: row_i64_to_usize(&row, "lifetime_unpriced_requests")?,
            });
        }

        Ok((now.to_rfc3339(), timezone, window_spec.key, aggregates))
    }

    pub async fn dashboard_breakdown_only(
        &self,
        timezone: Option<&str>,
        window_key: &str,
    ) -> anyhow::Result<(
        String,
        String,
        String,
        Vec<UsageBreakdownItem>,
        Vec<UsageBreakdownItem>,
    )> {
        let now = Utc::now();
        let (timezone, offset) = usage_dashboard_timezone(timezone);
        let window_spec = usage_dashboard_window_spec_for_key(now, offset, window_key);
        let specs = [window_spec.clone()];
        let mut status_breakdown = self
            .dashboard_breakdown(&specs, DashboardBreakdownColumn::Status)
            .await?;
        let mut usage_source_breakdown = self
            .dashboard_breakdown(&specs, DashboardBreakdownColumn::UsageSource)
            .await?;
        Ok((
            now.to_rfc3339(),
            timezone,
            window_spec.key.clone(),
            status_breakdown
                .remove(&window_spec.key)
                .unwrap_or_default(),
            usage_source_breakdown
                .remove(&window_spec.key)
                .unwrap_or_default(),
        ))
    }

    pub async fn dashboard_external_pool_billing_only(
        &self,
        timezone: Option<&str>,
        window_key: &str,
    ) -> anyhow::Result<(String, String, String, Vec<UsageExternalPoolBillingByPool>)> {
        let now = Utc::now();
        let (timezone, offset) = usage_dashboard_timezone(timezone);
        let window_spec = usage_dashboard_window_spec_for_key(now, offset, window_key);
        let specs = [window_spec.clone()];
        let mut billing = self.dashboard_external_pool_billing_by_pool(&specs).await?;
        Ok((
            now.to_rfc3339(),
            timezone,
            window_spec.key.clone(),
            billing.remove(&window_spec.key).unwrap_or_default(),
        ))
    }

    pub async fn external_pool_usage_risk(
        &self,
        query: UsageExternalPoolRiskQuery,
        cost_config: UsageExternalPoolRiskCostConfig,
    ) -> anyhow::Result<UsageExternalPoolRiskResponse> {
        if query.from >= query.to {
            anyhow::bail!("外部池 usage 风控查询时间范围无效");
        }

        let cost_target_multiplier = if cost_config.cost_floor_enabled {
            1.0 + (cost_config.cost_floor_margin_percent as f64 / 100.0)
        } else {
            1.0
        }
        .max(1.0);

        let mut tx = self.store.pool().begin().await?;
        configure_usage_dashboard_read_transaction(&mut tx).await?;

        let summary_row = external_pool_usage_risk_summary_query(&query, cost_target_multiplier)
            .build()
            .fetch_one(&mut *tx)
            .await?;
        let totals = external_pool_usage_risk_totals_from_row(&summary_row)?;
        let raw_cache = external_pool_usage_risk_cache_stats_from_row(&summary_row, "raw")?;
        let reported_cache =
            external_pool_usage_risk_cache_stats_from_row(&summary_row, "reported")?;
        let cost = external_pool_usage_risk_cost_stats_from_row(&summary_row)?;

        let bucket_rows = external_pool_usage_risk_bucket_query(&query, cost_target_multiplier)
            .build()
            .fetch_all(&mut *tx)
            .await?;
        let buckets = bucket_rows
            .into_iter()
            .map(external_pool_usage_risk_bucket_from_row)
            .collect::<anyhow::Result<Vec<_>>>()?;

        let group_rows = external_pool_usage_risk_group_query(&query, cost_target_multiplier)
            .build()
            .fetch_all(&mut *tx)
            .await?;
        let mut by_pool = Vec::new();
        let mut by_path = Vec::new();
        let mut by_model = Vec::new();
        for row in group_rows {
            let group_kind: String = row.try_get("group_kind")?;
            let group = external_pool_usage_risk_group_from_row(&row)?;
            match group_kind.as_str() {
                "pool" => by_pool.push(group),
                "path" => by_path.push(group),
                "model" => by_model.push(group),
                _ => {}
            }
        }

        let sample_rows = external_pool_usage_risk_sample_query(&query, cost_target_multiplier)
            .build()
            .fetch_all(&mut *tx)
            .await?;
        let samples = sample_rows
            .into_iter()
            .map(external_pool_usage_risk_sample_from_row)
            .collect::<anyhow::Result<Vec<_>>>()?;

        tx.commit().await?;

        Ok(UsageExternalPoolRiskResponse {
            generated_at: Utc::now().to_rfc3339(),
            timezone: query.timezone.clone(),
            window: UsageExternalPoolRiskWindow {
                key: query.window_key.clone(),
                label: query.window_label.clone(),
                from: query.from.to_rfc3339(),
                to: query.to.to_rfc3339(),
            },
            thresholds: UsageExternalPoolRiskThresholds {
                warning_tokens: query.warning_threshold_tokens,
                critical_tokens: query.critical_threshold_tokens,
                cost_floor_enabled: cost_config.cost_floor_enabled,
                cost_floor_margin_percent: cost_config.cost_floor_margin_percent,
                cost_target_multiplier,
            },
            filters: UsageExternalPoolRiskFilters {
                pool_id: query.pool_id,
                endpoint: query.endpoint.clone(),
                model: query.model.clone(),
                stream: query.stream,
            },
            totals,
            raw_cache,
            reported_cache,
            cost,
            buckets,
            by_pool,
            by_path,
            by_model,
            samples,
        })
    }

    async fn dashboard_windows(
        &self,
        specs: &[UsageDashboardWindowSpec],
        high_cache_threshold: i32,
    ) -> anyhow::Result<Vec<UsageDashboardWindow>> {
        if specs.is_empty() {
            return Ok(Vec::new());
        }

        let mut builder = QueryBuilder::<Postgres>::new("");
        push_dashboard_windows_cte(&mut builder, specs);
        push_dashboard_global_segments_cte(&mut builder);
        builder.push(
            r#"
            , rollup AS (
            SELECT
                w.key,
                w.label,
                w.from_at,
                w.to_at,
                COALESCE(SUM(s.requests), 0)::bigint AS total_requests,
                COALESCE(SUM(s.success_requests), 0)::bigint AS success_requests,
                COALESCE(SUM(s.error_requests), 0)::bigint AS error_requests,
                COALESCE(SUM(s.stream_requests), 0)::bigint AS stream_requests,
                COALESCE(SUM(s.non_stream_requests), 0)::bigint AS non_stream_requests,
                COALESCE(SUM(s.total_input_tokens), 0)::bigint AS total_input_tokens,
                COALESCE(SUM(s.billable_input_tokens), 0)::bigint AS billable_input_tokens,
                COALESCE(SUM(s.total_output_tokens), 0)::bigint AS total_output_tokens,
                COALESCE(SUM(s.total_cache_read_input_tokens), 0)::bigint AS total_cache_read_input_tokens,
                COALESCE(SUM(s.total_cache_creation_input_tokens), 0)::bigint AS total_cache_creation_input_tokens,
                COALESCE(SUM(s.total_estimated_cost_usd), 0)::double precision AS total_estimated_cost_usd,
                COALESCE(SUM(
                    CASE
                        WHEN COALESCE(s.total_original_cost_usd, 0) <> 0
                        THEN s.total_original_cost_usd
                        ELSE COALESCE(s.total_estimated_cost_usd, 0)
                    END
                ), 0)::double precision AS total_original_cost_usd,
                COALESCE(SUM(s.total_kiro_metering_usage), 0)::double precision
                    AS total_kiro_metering_usage,
                COALESCE(SUM(s.priced_requests), 0)::bigint AS priced_requests,
                COALESCE(SUM(s.unpriced_requests), 0)::bigint AS unpriced_requests,
                CASE
                    WHEN COALESCE(SUM(s.duration_ms_count), 0) > 0
                    THEN COALESCE(SUM(s.duration_ms_sum), 0)::double precision
                         / SUM(s.duration_ms_count)::double precision
                    ELSE 0
                END AS average_duration_ms,
                COALESCE(SUM(s.sticky_bound_requests), 0)::bigint AS sticky_bound_requests,
                COALESCE(SUM(s.fallback_from_sticky_requests), 0)::bigint AS fallback_from_sticky_requests,
                COALESCE(SUM(s.simulated_requests), 0)::bigint AS simulated_requests,
                COALESCE(SUM(s.upstream_metadata_requests), 0)::bigint AS upstream_metadata_requests,
                COALESCE(SUM(s.external_pool_requests), 0)::bigint AS external_pool_requests,
                COALESCE(SUM(s.external_pool_priced_requests), 0)::bigint AS external_pool_priced_requests,
                COALESCE(SUM(s.external_pool_unpriced_requests), 0)::bigint AS external_pool_unpriced_requests,
                COALESCE(SUM(s.external_pool_cost_floor_applied_requests), 0)::bigint AS external_pool_cost_floor_applied_requests,
                COALESCE(SUM(s.external_pool_raw_cost_usd), 0)::double precision AS external_pool_raw_cost_usd,
                COALESCE(SUM(
                    CASE
                        WHEN COALESCE(s.external_pool_shaped_cost_usd, 0) <> 0
                        THEN s.external_pool_shaped_cost_usd
                        ELSE COALESCE(s.external_pool_reported_cost_usd, 0)
                    END
                ), 0)::double precision AS external_pool_shaped_cost_usd,
                COALESCE(SUM(
                    CASE
                        WHEN COALESCE(s.external_pool_uplifted_cost_usd, 0) <> 0
                        THEN s.external_pool_uplifted_cost_usd
                        ELSE COALESCE(s.external_pool_reported_cost_usd, 0)
                    END
                ), 0)::double precision AS external_pool_uplifted_cost_usd,
                COALESCE(SUM(
                    CASE
                        WHEN COALESCE(s.external_pool_profit_usd, 0) <> 0
                        THEN s.external_pool_profit_usd
                        WHEN COALESCE(s.external_pool_reported_cost_usd, 0) <> 0
                          OR COALESCE(s.external_pool_raw_cost_usd, 0) <> 0
                        THEN COALESCE(s.external_pool_reported_cost_usd, 0) - COALESCE(s.external_pool_raw_cost_usd, 0)
                        ELSE 0
                    END
                ), 0)::double precision AS external_pool_profit_usd,
                COALESCE(SUM(s.external_pool_reported_cost_usd), 0)::double precision AS external_pool_reported_cost_usd,
                COALESCE(SUM(s.external_pool_billable_cost_usd), 0)::double precision AS external_pool_billable_cost_usd,
                COALESCE(SUM(s.external_pool_cost_floor_delta_usd), 0)::double precision AS external_pool_cost_floor_delta_usd
            FROM window_bounds w
            LEFT JOIN dashboard_global_segments s ON s.window_key = w.key
            GROUP BY w.key, w.label, w.from_at, w.to_at, w.ord
            ), high_cache_segments AS (
            SELECT
                w.key AS window_key,
                c.requests
            FROM window_bounds w
            JOIN usage_cache_read_rollup_time_buckets c
              ON c.bucket_start >= w.full_from_at
             AND c.bucket_start < w.full_to_at
             AND c.cache_read_input_tokens >=
            "#,
        );
        builder.push_bind(high_cache_threshold);
        builder.push(
            r#"
            UNION ALL
            SELECT
                r.window_key,
                COUNT(*)::bigint AS requests
            FROM window_boundary_records r
            WHERE GREATEST(r.cache_read_input_tokens, 0) >=
            "#,
        );
        builder.push_bind(high_cache_threshold);
        builder.push(
            r#"
            GROUP BY r.window_key
            ), high_cache AS (
            SELECT
                window_key AS key,
                COALESCE(SUM(requests), 0)::bigint AS high_cache_requests
            FROM high_cache_segments
            GROUP BY window_key
            ), duration_count_segments AS (
            SELECT
                w.key AS window_key,
                d.duration_ms,
                d.requests
            FROM window_bounds w
            JOIN usage_duration_rollup_time_buckets d
              ON d.bucket_start >= w.full_from_at
             AND d.bucket_start < w.full_to_at
            UNION ALL
            SELECT
                r.window_key,
                LEAST(r.dashboard_duration_ms, 2147483647)::bigint AS duration_ms,
                COUNT(*)::bigint AS requests
            FROM window_boundary_records r
            GROUP BY r.window_key, LEAST(r.dashboard_duration_ms, 2147483647)
            ), duration_counts AS (
            SELECT
                window_key AS key,
                duration_ms,
                SUM(requests)::numeric AS requests
            FROM duration_count_segments
            GROUP BY window_key, duration_ms
            HAVING SUM(requests) > 0
            ), duration_ranked AS (
            SELECT
                key,
                duration_ms,
                SUM(requests) OVER (
                    PARTITION BY key
                    ORDER BY duration_ms
                    ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW
                ) AS cumulative_requests,
                SUM(requests) OVER (PARTITION BY key) AS total_requests
            FROM duration_counts
            ), duration_p95 AS (
            SELECT
                key,
                MIN(duration_ms)::bigint AS p95_duration_ms
            FROM duration_ranked
            WHERE cumulative_requests::numeric * 100::numeric
                >= total_requests::numeric * 95::numeric
            GROUP BY key
            )
            SELECT
                r.key,
                r.label,
                r.from_at,
                r.to_at,
                r.total_requests,
                r.success_requests,
                r.error_requests,
                r.stream_requests,
                r.non_stream_requests,
                COALESCE(h.high_cache_requests, 0)::bigint AS high_cache_requests,
                r.total_input_tokens,
                r.billable_input_tokens,
                r.total_output_tokens,
                r.total_cache_read_input_tokens,
                r.total_cache_creation_input_tokens,
                r.total_estimated_cost_usd,
                r.total_original_cost_usd,
                r.total_kiro_metering_usage,
                r.priced_requests,
                r.unpriced_requests,
                r.average_duration_ms,
                COALESCE(p.p95_duration_ms, 0)::bigint AS p95_duration_ms,
                r.sticky_bound_requests,
                r.fallback_from_sticky_requests,
                r.simulated_requests,
                r.upstream_metadata_requests,
                r.external_pool_requests,
                r.external_pool_priced_requests,
                r.external_pool_unpriced_requests,
                r.external_pool_cost_floor_applied_requests,
                r.external_pool_raw_cost_usd,
                r.external_pool_shaped_cost_usd,
                r.external_pool_uplifted_cost_usd,
                r.external_pool_profit_usd,
                r.external_pool_reported_cost_usd,
                r.external_pool_billable_cost_usd,
                r.external_pool_cost_floor_delta_usd
            FROM rollup r
            LEFT JOIN high_cache h ON h.key = r.key
            LEFT JOIN duration_p95 p ON p.key = r.key
            JOIN window_bounds w ON w.key = r.key
            ORDER BY w.ord
            "#,
        );

        let mut tx = self.store.pool().begin().await?;
        configure_usage_dashboard_read_transaction(&mut tx).await?;
        let rows = builder.build().fetch_all(&mut *tx).await?;
        tx.commit().await?;
        rows.into_iter()
            .map(dashboard_window_from_row)
            .collect::<anyhow::Result<Vec<_>>>()
    }

    async fn dashboard_windows_with_basic_fallback(
        &self,
        specs: &[UsageDashboardWindowSpec],
        high_cache_threshold: i32,
    ) -> anyhow::Result<Vec<UsageDashboardWindow>> {
        match self.dashboard_windows(specs, high_cache_threshold).await {
            Ok(windows) => Ok(windows),
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "usage dashboard 精确窗口聚合失败，降级为基础窗口聚合"
                );
                self.dashboard_windows_basic_from_series(specs).await
            }
        }
    }

    async fn dashboard_windows_basic_from_series(
        &self,
        specs: &[UsageDashboardWindowSpec],
    ) -> anyhow::Result<Vec<UsageDashboardWindow>> {
        let points = self.dashboard_series(specs).await?;
        Ok(points
            .into_iter()
            .map(usage_dashboard_window_from_series_point)
            .collect())
    }

    async fn populate_dashboard_window_details(
        &self,
        specs: &[UsageDashboardWindowSpec],
        windows: &mut [UsageDashboardWindow],
    ) -> anyhow::Result<()> {
        if specs.is_empty() || windows.is_empty() {
            return Ok(());
        }

        let mut status_breakdown = self
            .dashboard_breakdown(specs, DashboardBreakdownColumn::Status)
            .await?;
        let mut usage_source_breakdown = self
            .dashboard_breakdown(specs, DashboardBreakdownColumn::UsageSource)
            .await?;
        let mut external_pool_billing = self.dashboard_external_pool_billing_by_pool(specs).await?;

        for window in windows {
            window.summary.status_breakdown =
                status_breakdown.remove(&window.key).unwrap_or_default();
            window.summary.usage_source_breakdown = usage_source_breakdown
                .remove(&window.key)
                .unwrap_or_default();
            window.summary.external_pool_billing_by_pool = external_pool_billing
                .remove(&window.key)
                .unwrap_or_default();
        }
        Ok(())
    }

    async fn dashboard_breakdown(
        &self,
        specs: &[UsageDashboardWindowSpec],
        column: DashboardBreakdownColumn,
    ) -> anyhow::Result<HashMap<String, Vec<UsageBreakdownItem>>> {
        if specs.is_empty() {
            return Ok(HashMap::new());
        }

        let item_dimension = column.rollup_dimension();
        let mut builder = QueryBuilder::<Postgres>::new("");
        push_dashboard_windows_cte(&mut builder, specs);
        builder.push(
            r#", breakdown_count_segments AS (
            SELECT
                   w.key AS window_key,
                   b.dimension_key AS item_key,
                   b.requests
            FROM window_bounds w
            JOIN usage_rollup_time_buckets b
              ON b.dimension = "#,
        );
        builder.push_bind(item_dimension);
        builder.push(
            r#"
             AND b.bucket_start >= w.full_from_at
             AND b.bucket_start < w.full_to_at
            UNION ALL
            SELECT
                r.window_key,
            "#,
        );
        builder.push(column.detail_column());
        builder.push(
            r#" AS item_key,
                COUNT(*)::bigint AS requests
            FROM window_boundary_records r
            GROUP BY r.window_key, "#,
        );
        builder.push(column.detail_column());
        builder.push(
            r#"
            ), breakdown_counts AS (
            SELECT
                window_key,
                item_key,
                SUM(requests)::bigint AS requests
            FROM breakdown_count_segments
            GROUP BY window_key, item_key
            ), breakdown_totals AS (
            SELECT
                window_key,
                SUM(requests)::bigint AS total_requests
            FROM breakdown_counts
            GROUP BY window_key
            )
            SELECT
                c.window_key,
                c.item_key,
                c.requests,
                COALESCE(t.total_requests, 0)::bigint AS total_requests
            FROM breakdown_counts c
            JOIN breakdown_totals t ON t.window_key = c.window_key
            JOIN window_bounds w ON w.key = c.window_key
            WHERE c.requests > 0
            ORDER BY w.ord, c.requests DESC, c.item_key
            "#,
        );

        let mut tx = self.store.pool().begin().await?;
        configure_usage_dashboard_read_transaction(&mut tx).await?;
        let rows = builder.build().fetch_all(&mut *tx).await?;
        tx.commit().await?;
        let mut grouped: HashMap<String, Vec<UsageBreakdownItem>> = HashMap::new();
        for row in rows {
            let window_key: String = row.try_get("window_key")?;
            let item_key: String = row.try_get("item_key")?;
            let requests = row_i64_to_usize(&row, "requests")?;
            let total_requests = row_i64_to_usize(&row, "total_requests")?;
            grouped
                .entry(window_key)
                .or_default()
                .push(UsageBreakdownItem {
                    label: column.label(&item_key),
                    key: item_key,
                    requests,
                    ratio: usage_ratio(requests, total_requests),
                });
        }
        Ok(grouped)
    }

    async fn dashboard_series(
        &self,
        specs: &[UsageDashboardWindowSpec],
    ) -> anyhow::Result<Vec<UsageSeriesPoint>> {
        if specs.is_empty() {
            return Ok(Vec::new());
        }

        let mut builder = QueryBuilder::<Postgres>::new("");
        push_dashboard_windows_cte(&mut builder, specs);
        push_dashboard_global_segments_cte(&mut builder);
        builder.push(
            r#"
            SELECT
                w.key,
                w.label,
                w.from_at,
                w.to_at,
                COALESCE(SUM(s.requests), 0)::bigint AS requests,
                COALESCE(SUM(s.success_requests), 0)::bigint AS success_requests,
                COALESCE(SUM(s.error_requests), 0)::bigint AS error_requests,
                COALESCE(SUM(s.total_input_tokens), 0)::bigint AS total_input_tokens,
                COALESCE(SUM(s.billable_input_tokens), 0)::bigint AS billable_input_tokens,
                COALESCE(SUM(s.total_output_tokens), 0)::bigint AS total_output_tokens,
                COALESCE(SUM(s.total_estimated_cost_usd), 0)::double precision AS total_estimated_cost_usd,
                COALESCE(SUM(
                    CASE
                        WHEN COALESCE(s.total_original_cost_usd, 0) <> 0
                        THEN s.total_original_cost_usd
                        ELSE COALESCE(s.total_estimated_cost_usd, 0)
                    END
                ), 0)::double precision AS total_original_cost_usd,
                COALESCE(SUM(s.total_kiro_metering_usage), 0)::double precision
                    AS total_kiro_metering_usage
            FROM window_bounds w
            LEFT JOIN dashboard_global_segments s ON s.window_key = w.key
            GROUP BY w.key, w.label, w.from_at, w.to_at, w.ord
            ORDER BY w.ord
            "#,
        );
        let mut tx = self.store.pool().begin().await?;
        configure_usage_dashboard_read_transaction(&mut tx).await?;
        let rows = builder.build().fetch_all(&mut *tx).await?;
        tx.commit().await?;
        rows.into_iter()
            .map(series_point_from_row)
            .collect::<anyhow::Result<Vec<_>>>()
    }

    async fn dashboard_external_pool_billing_by_pool(
        &self,
        specs: &[UsageDashboardWindowSpec],
    ) -> anyhow::Result<HashMap<String, Vec<UsageExternalPoolBillingByPool>>> {
        if specs.is_empty() {
            return Ok(HashMap::new());
        }

        let mut builder = QueryBuilder::<Postgres>::new("");
        push_dashboard_windows_cte(&mut builder, specs);
        push_dashboard_external_pool_segments_cte(&mut builder);
        builder.push(
            r#"
            SELECT
                w.key AS window_key,
                s.pool_id,
                NULLIF(MAX(s.pool_name), '') AS pool_name,
                COALESCE(SUM(s.external_pool_requests), 0)::bigint AS requests,
                COALESCE(SUM(s.external_pool_priced_requests), 0)::bigint AS priced_requests,
                COALESCE(SUM(s.external_pool_unpriced_requests), 0)::bigint AS unpriced_requests,
                COALESCE(SUM(s.external_pool_cost_floor_applied_requests), 0)::bigint AS cost_floor_applied_requests,
                COALESCE(SUM(s.external_pool_raw_cost_usd), 0)::double precision AS raw_cost_usd,
                COALESCE(SUM(
                    CASE
                        WHEN COALESCE(s.external_pool_shaped_cost_usd, 0) <> 0
                        THEN s.external_pool_shaped_cost_usd
                        ELSE COALESCE(s.external_pool_reported_cost_usd, 0)
                    END
                ), 0)::double precision AS shaped_cost_usd,
                COALESCE(SUM(
                    CASE
                        WHEN COALESCE(s.external_pool_uplifted_cost_usd, 0) <> 0
                        THEN s.external_pool_uplifted_cost_usd
                        ELSE COALESCE(s.external_pool_reported_cost_usd, 0)
                    END
                ), 0)::double precision AS uplifted_cost_usd,
                COALESCE(SUM(
                    CASE
                        WHEN COALESCE(s.external_pool_profit_usd, 0) <> 0
                        THEN s.external_pool_profit_usd
                        WHEN COALESCE(s.external_pool_reported_cost_usd, 0) <> 0
                          OR COALESCE(s.external_pool_raw_cost_usd, 0) <> 0
                        THEN COALESCE(s.external_pool_reported_cost_usd, 0) - COALESCE(s.external_pool_raw_cost_usd, 0)
                        ELSE 0
                    END
                ), 0)::double precision AS profit_usd,
                COALESCE(SUM(s.external_pool_reported_cost_usd), 0)::double precision AS reported_cost_usd,
                COALESCE(SUM(s.external_pool_billable_cost_usd), 0)::double precision AS billable_cost_usd,
                COALESCE(SUM(s.external_pool_cost_floor_delta_usd), 0)::double precision AS cost_floor_delta_usd
            FROM window_bounds w
            JOIN dashboard_external_pool_segments s ON s.window_key = w.key
            GROUP BY w.key, w.ord, s.pool_id
            HAVING COALESCE(SUM(s.external_pool_requests), 0) > 0
            ORDER BY w.ord, uplifted_cost_usd DESC, requests DESC, pool_id
            "#,
        );

        let mut tx = self.store.pool().begin().await?;
        configure_usage_dashboard_read_transaction(&mut tx).await?;
        let rows = builder.build().fetch_all(&mut *tx).await?;
        tx.commit().await?;
        let mut grouped: HashMap<String, Vec<UsageExternalPoolBillingByPool>> = HashMap::new();
        for row in rows {
            let window_key: String = row.try_get("window_key")?;
            let pool_key: String = row.try_get("pool_id")?;
            let pool_name = row
                .try_get::<Option<String>, _>("pool_name")?
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| format!("#{}", pool_key));
            grouped
                .entry(window_key)
                .or_default()
                .push(UsageExternalPoolBillingByPool {
                    pool_id: pool_key.parse::<u64>().unwrap_or(0),
                    pool_name,
                    requests: row_i64_to_usize(&row, "requests")?,
                    priced_requests: row_i64_to_usize(&row, "priced_requests")?,
                    unpriced_requests: row_i64_to_usize(&row, "unpriced_requests")?,
                    cost_floor_applied_requests: row_i64_to_usize(
                        &row,
                        "cost_floor_applied_requests",
                    )?,
                    raw_cost_usd: row.try_get("raw_cost_usd")?,
                    shaped_cost_usd: row.try_get("shaped_cost_usd")?,
                    uplifted_cost_usd: row.try_get("uplifted_cost_usd")?,
                    profit_usd: row.try_get("profit_usd")?,
                    reported_cost_usd: row.try_get("reported_cost_usd")?,
                    billable_cost_usd: row.try_get("billable_cost_usd")?,
                    cost_floor_delta_usd: row.try_get("cost_floor_delta_usd")?,
                });
        }
        Ok(grouped)
    }

    async fn dashboard_top_aggregates(
        &self,
        group: DashboardTopGroup,
    ) -> anyhow::Result<DashboardTopAggregateResult> {
        let mut builder = QueryBuilder::<Postgres>::new(
            r#"
            SELECT
                dimension_key AS key,
                NULLIF(dimension_label, '') AS label,
                requests::bigint AS requests,
                error_requests::bigint AS error_requests,
                total_input_tokens::bigint AS total_input_tokens,
                billable_input_tokens::bigint AS billable_input_tokens,
                total_output_tokens::bigint AS total_output_tokens,
                total_cache_read_input_tokens::bigint AS total_cache_read_input_tokens,
                total_cache_creation_input_tokens::bigint AS total_cache_creation_input_tokens,
                total_estimated_cost_usd::double precision AS total_estimated_cost_usd,
                CASE
                    WHEN COALESCE(total_original_cost_usd, 0) <> 0
                    THEN total_original_cost_usd
                    ELSE COALESCE(total_estimated_cost_usd, 0)
                END::double precision AS total_original_cost_usd,
                COALESCE(total_kiro_metering_usage, 0)::double precision
                    AS total_kiro_metering_usage,
                COUNT(*) OVER()::bigint AS total_dimension_count
            FROM usage_rollup_totals
            WHERE dimension = "#,
        );
        builder.push_bind(group.rollup_dimension());
        builder.push(group.rollup_extra_where());
        builder.push(" AND requests > 0 ");
        let order_by = if matches!(group, DashboardTopGroup::Error) {
            " ORDER BY error_requests DESC, requests DESC, total_estimated_cost_usd DESC, key LIMIT 10"
        } else {
            " ORDER BY total_estimated_cost_usd DESC, requests DESC, total_input_tokens DESC, key LIMIT 10"
        };
        builder.push(order_by);

        let mut tx = self.store.pool().begin().await?;
        configure_usage_dashboard_read_transaction(&mut tx).await?;
        let rows = builder.build().fetch_all(&mut *tx).await?;
        tx.commit().await?;
        let total = rows
            .first()
            .map(|row| row_i64_to_usize(row, "total_dimension_count"))
            .transpose()?
            .unwrap_or(0);
        let items = rows
            .into_iter()
            .map(usage_top_aggregate_from_row)
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(DashboardTopAggregateResult { items, total })
    }

    async fn dashboard_top_aggregates_for_window(
        &self,
        specs: &[UsageDashboardWindowSpec],
        group: DashboardTopGroup,
    ) -> anyhow::Result<DashboardTopAggregateResult> {
        if specs.is_empty() {
            return Ok(DashboardTopAggregateResult {
                items: Vec::new(),
                total: 0,
            });
        }

        let item_dimension = group.rollup_dimension();
        let mut builder = QueryBuilder::<Postgres>::new("");
        push_dashboard_windows_cte(&mut builder, specs);
        builder.push(
            r#", top_metric_segments AS (
            SELECT
                w.key AS window_key,
                b.dimension_key AS item_key,
                NULLIF(BTRIM(b.dimension_label), '') AS item_label,
                b.requests,
                b.error_requests,
                b.total_input_tokens,
                b.billable_input_tokens,
                b.total_output_tokens,
                b.total_cache_read_input_tokens,
                b.total_cache_creation_input_tokens,
                b.total_estimated_cost_usd,
                CASE
                    WHEN COALESCE(b.total_original_cost_usd, 0) <> 0
                    THEN b.total_original_cost_usd
                    ELSE COALESCE(b.total_estimated_cost_usd, 0)
                END::double precision AS total_original_cost_usd,
                COALESCE(b.total_kiro_metering_usage, 0)::double precision
                    AS total_kiro_metering_usage
            FROM window_bounds w
            JOIN usage_rollup_time_buckets b
              ON b.dimension = "#,
        );
        builder.push_bind(item_dimension);
        builder.push(
            r#"
             AND b.bucket_start >= w.full_from_at
             AND b.bucket_start < w.full_to_at
            UNION ALL
            SELECT
                r.window_key,
            "#,
        );
        builder.push(group.detail_key_column());
        builder.push(
            r#" AS item_key,
            "#,
        );
        builder.push(group.detail_label_column());
        builder.push(
            r#" AS item_label,
                COUNT(*)::bigint AS requests,
                COUNT(*) FILTER (WHERE r.status <> 'success')::bigint AS error_requests,
                COALESCE(SUM(r.total_input_tokens), 0)::bigint AS total_input_tokens,
                COALESCE(SUM(r.billable_input_tokens), 0)::bigint AS billable_input_tokens,
                COALESCE(SUM(r.output_tokens), 0)::bigint AS total_output_tokens,
                COALESCE(SUM(r.cache_read_input_tokens), 0)::bigint AS total_cache_read_input_tokens,
                COALESCE(SUM(r.cache_creation_input_tokens), 0)::bigint AS total_cache_creation_input_tokens,
                COALESCE(SUM(r.estimated_cost_usd), 0)::double precision AS total_estimated_cost_usd,
                COALESCE(SUM(
                    CASE
                        WHEN COALESCE(r.original_cost_usd, 0) <> 0
                        THEN r.original_cost_usd
                        ELSE COALESCE(r.estimated_cost_usd, 0)
                    END
                ), 0)::double precision AS total_original_cost_usd,
                COALESCE(SUM(r.kiro_metering_usage), 0)::double precision
                    AS total_kiro_metering_usage
            FROM window_boundary_records r
            "#,
        );
        if let Some(filter) = group.detail_filter() {
            builder.push(" WHERE ");
            builder.push(filter);
        }
        builder.push(" GROUP BY r.window_key, ");
        builder.push(group.detail_key_column());
        builder.push(", ");
        builder.push(group.detail_label_column());
        builder.push(
            r#"
            ), top_metric_totals AS (
            SELECT
                item_key AS key,
                NULLIF(MAX(item_label), '') AS label,
                SUM(requests)::bigint AS requests,
                SUM(error_requests)::bigint AS error_requests,
                SUM(total_input_tokens)::bigint AS total_input_tokens,
                SUM(billable_input_tokens)::bigint AS billable_input_tokens,
                SUM(total_output_tokens)::bigint AS total_output_tokens,
                SUM(total_cache_read_input_tokens)::bigint AS total_cache_read_input_tokens,
                SUM(total_cache_creation_input_tokens)::bigint AS total_cache_creation_input_tokens,
                SUM(total_estimated_cost_usd)::double precision AS total_estimated_cost_usd,
                SUM(total_original_cost_usd)::double precision AS total_original_cost_usd,
                SUM(total_kiro_metering_usage)::double precision
                    AS total_kiro_metering_usage
            FROM top_metric_segments
            GROUP BY item_key
            )
            SELECT
                key,
                label,
                requests,
                error_requests,
                total_input_tokens,
                billable_input_tokens,
                total_output_tokens,
                total_cache_read_input_tokens,
                total_cache_creation_input_tokens,
                total_estimated_cost_usd,
                total_original_cost_usd,
                total_kiro_metering_usage,
                COUNT(*) OVER()::bigint AS total_dimension_count
            FROM top_metric_totals
            WHERE requests > 0
            ORDER BY
                "#,
        );
        if matches!(group, DashboardTopGroup::Error) {
            builder.push(
                r#"error_requests DESC, requests DESC, total_estimated_cost_usd DESC, key
            LIMIT 10
            "#,
            );
        } else {
            builder.push(
                r#"total_estimated_cost_usd DESC, requests DESC, total_input_tokens DESC, key
            LIMIT 10
            "#,
            );
        }

        let mut tx = self.store.pool().begin().await?;
        configure_usage_dashboard_read_transaction(&mut tx).await?;
        let rows = builder.build().fetch_all(&mut *tx).await?;
        tx.commit().await?;
        let total = rows
            .first()
            .map(|row| row_i64_to_usize(row, "total_dimension_count"))
            .transpose()?
            .unwrap_or(0);
        let items = rows
            .into_iter()
            .map(usage_top_aggregate_from_row)
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(DashboardTopAggregateResult { items, total })
    }

    pub async fn credential_cost_summary(
        &self,
    ) -> anyhow::Result<HashMap<u64, CredentialCostSummary>> {
        let rows = sqlx::query(
            r#"
            SELECT
                credential_id,
                estimated_cost_usd,
                CASE
                    WHEN COALESCE(original_cost_usd, 0) <> 0
                    THEN original_cost_usd
                    ELSE COALESCE(estimated_cost_usd, 0)
                END::double precision AS original_cost_usd,
                kiro_metering_usage,
                priced_requests,
                unpriced_requests
            FROM usage_credential_cost_summary
            WHERE requests > 0
            "#,
        )
        .fetch_all(self.store.pool())
        .await?;

        let mut summaries = HashMap::with_capacity(rows.len());
        for row in rows {
            let credential_id: i64 = row.try_get("credential_id")?;
            summaries.insert(
                credential_id as u64,
                CredentialCostSummary {
                    estimated_cost_usd: row.try_get("estimated_cost_usd")?,
                    original_cost_usd: row.try_get("original_cost_usd")?,
                    kiro_metering_usage: row.try_get("kiro_metering_usage")?,
                    priced_requests: row_i64_to_usize(&row, "priced_requests")?,
                    unpriced_requests: row_i64_to_usize(&row, "unpriced_requests")?,
                },
            );
        }
        Ok(summaries)
    }

    pub async fn credential_cost_summary_for_ids(
        &self,
        ids: &[u64],
    ) -> anyhow::Result<HashMap<u64, CredentialCostSummary>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let credential_ids: Vec<i64> = ids.iter().map(|id| *id as i64).collect();
        let rows = sqlx::query(
            r#"
            SELECT
                credential_id,
                estimated_cost_usd,
                CASE
                    WHEN COALESCE(original_cost_usd, 0) <> 0
                    THEN original_cost_usd
                    ELSE COALESCE(estimated_cost_usd, 0)
                END::double precision AS original_cost_usd,
                kiro_metering_usage,
                priced_requests,
                unpriced_requests
            FROM usage_credential_cost_summary
            WHERE requests > 0 AND credential_id = ANY($1)
            "#,
        )
        .bind(&credential_ids)
        .fetch_all(self.store.pool())
        .await?;

        let mut summaries = HashMap::with_capacity(rows.len());
        for row in rows {
            let credential_id: i64 = row.try_get("credential_id")?;
            summaries.insert(
                credential_id as u64,
                CredentialCostSummary {
                    estimated_cost_usd: row.try_get("estimated_cost_usd")?,
                    original_cost_usd: row.try_get("original_cost_usd")?,
                    kiro_metering_usage: row.try_get("kiro_metering_usage")?,
                    priced_requests: row_i64_to_usize(&row, "priced_requests")?,
                    unpriced_requests: row_i64_to_usize(&row, "unpriced_requests")?,
                },
            );
        }
        Ok(summaries)
    }

    async fn top_credential_aggregates(&self) -> anyhow::Result<Vec<UsageAggregate>> {
        let rows = sqlx::query(
            r#"
            SELECT
                dimension_key AS key,
                NULLIF(dimension_label, '') AS label,
                requests,
                total_cache_read_input_tokens AS cache_read_input_tokens,
                total_cache_creation_input_tokens AS cache_creation_input_tokens,
                total_estimated_cost_usd AS estimated_cost_usd,
                CASE
                    WHEN COALESCE(total_original_cost_usd, 0) <> 0
                    THEN total_original_cost_usd
                    ELSE COALESCE(total_estimated_cost_usd, 0)
                END AS original_cost_usd
            FROM usage_rollup_totals
            WHERE dimension = 'credential' AND requests > 0
            ORDER BY estimated_cost_usd DESC, requests DESC, cache_read_input_tokens DESC
            LIMIT 10
            "#,
        )
        .fetch_all(self.store.pool())
        .await?;
        rows.into_iter().map(usage_aggregate_from_row).collect()
    }

    async fn top_conversation_aggregates(&self) -> anyhow::Result<Vec<UsageAggregate>> {
        let rows = sqlx::query(
            r#"
            SELECT
                dimension_key AS key,
                NULLIF(dimension_label, '') AS label,
                requests,
                total_cache_read_input_tokens AS cache_read_input_tokens,
                total_cache_creation_input_tokens AS cache_creation_input_tokens,
                total_estimated_cost_usd AS estimated_cost_usd,
                CASE
                    WHEN COALESCE(total_original_cost_usd, 0) <> 0
                    THEN total_original_cost_usd
                    ELSE COALESCE(total_estimated_cost_usd, 0)
                END AS original_cost_usd
            FROM usage_rollup_totals
            WHERE dimension = 'conversation' AND requests > 0
            ORDER BY estimated_cost_usd DESC, requests DESC, cache_read_input_tokens DESC
            LIMIT 10
            "#,
        )
        .fetch_all(self.store.pool())
        .await?;
        rows.into_iter().map(usage_aggregate_from_row).collect()
    }

    async fn load_matching(
        &self,
        query: UsageRecordQuery,
        offset: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<UsageRecord>> {
        let mut builder = QueryBuilder::<Postgres>::new("SELECT data FROM usage_records");
        push_usage_filters(&mut builder, &query);
        builder.push(" ORDER BY created_at DESC, id DESC OFFSET ");
        builder.push_bind(usize_to_i64(offset));
        builder.push(" LIMIT ");
        builder.push_bind(usize_to_i64(limit));

        let rows = builder.build().fetch_all(self.store.pool()).await?;
        let mut records = Vec::with_capacity(rows.len());
        for row in rows {
            let value: serde_json::Value = row.try_get("data")?;
            let mut record: UsageRecord = serde_json::from_value(value)?;
            apply_usage_record_legacy_cost_compatibility(&mut record);
            records.push(record);
        }
        Ok(records)
    }

    async fn count_matching(&self, query: UsageRecordQuery) -> anyhow::Result<usize> {
        let mut builder =
            QueryBuilder::<Postgres>::new("SELECT COUNT(*)::bigint AS count FROM usage_records");
        push_usage_filters(&mut builder, &query);
        let row = builder.build().fetch_one(self.store.pool()).await?;
        row_i64_to_usize(&row, "count")
    }
}

fn external_pool_usage_risk_summary_query<'a>(
    query: &'a UsageExternalPoolRiskQuery,
    cost_target_multiplier: f64,
) -> QueryBuilder<'a, Postgres> {
    let mut builder = QueryBuilder::<Postgres>::new("");
    push_external_pool_usage_risk_scored_cte(&mut builder, query, cost_target_multiplier);
    builder.push(
        r#"
        SELECT
            COUNT(*)::bigint AS records,
            COUNT(*) FILTER (WHERE status = 'success')::bigint AS success_records,
            COUNT(*) FILTER (WHERE status <> 'success')::bigint AS error_records,
            COUNT(*) FILTER (WHERE stream)::bigint AS stream_records,
            COUNT(*) FILTER (WHERE NOT stream)::bigint AS non_stream_records,
            COUNT(*) FILTER (WHERE pricing_available)::bigint AS priced_records,
            COUNT(*) FILTER (WHERE NOT pricing_available)::bigint AS unpriced_records,
            COUNT(*) FILTER (WHERE raw_usage_present)::bigint AS raw_usage_records,
            COUNT(*) FILTER (WHERE reported_usage_present)::bigint AS reported_usage_records,
            COUNT(*) FILTER (WHERE NOT external_pool_billing_present)::bigint
                AS missing_external_pool_billing_records,
            COUNT(*) FILTER (WHERE reported_output_tokens = 0)::bigint AS output_zero_records,

            COALESCE(MIN(raw_cache_read_input_tokens), 0)::bigint AS raw_min_read_tokens,
            COALESCE(MAX(raw_cache_read_input_tokens), 0)::bigint AS raw_max_read_tokens,
            COALESCE(AVG(raw_cache_read_input_tokens), 0)::double precision
                AS raw_avg_read_tokens,
            COALESCE(SUM(raw_cache_read_input_tokens), 0)::bigint AS raw_total_read_tokens,
            COALESCE(MIN(raw_cache_creation_input_tokens), 0)::bigint AS raw_min_write_tokens,
            COALESCE(MAX(raw_cache_creation_input_tokens), 0)::bigint AS raw_max_write_tokens,
            COALESCE(AVG(raw_cache_creation_input_tokens), 0)::double precision
                AS raw_avg_write_tokens,
            COALESCE(SUM(raw_cache_creation_input_tokens), 0)::bigint
                AS raw_total_write_tokens,
            COUNT(*) FILTER (WHERE raw_cache_read_input_tokens >= warning_threshold)::bigint
                AS raw_read_warning_count,
            COUNT(*) FILTER (WHERE raw_cache_creation_input_tokens >= warning_threshold)::bigint
                AS raw_write_warning_count,
            COUNT(*) FILTER (
                WHERE raw_cache_read_input_tokens >= warning_threshold
                   OR raw_cache_creation_input_tokens >= warning_threshold
            )::bigint AS raw_either_warning_count,
            COUNT(*) FILTER (WHERE raw_cache_read_input_tokens >= critical_threshold)::bigint
                AS raw_read_critical_count,
            COUNT(*) FILTER (WHERE raw_cache_creation_input_tokens >= critical_threshold)::bigint
                AS raw_write_critical_count,
            COUNT(*) FILTER (
                WHERE raw_cache_read_input_tokens >= critical_threshold
                   OR raw_cache_creation_input_tokens >= critical_threshold
            )::bigint AS raw_either_critical_count,

            COALESCE(MIN(reported_cache_read_input_tokens), 0)::bigint
                AS reported_min_read_tokens,
            COALESCE(MAX(reported_cache_read_input_tokens), 0)::bigint
                AS reported_max_read_tokens,
            COALESCE(AVG(reported_cache_read_input_tokens), 0)::double precision
                AS reported_avg_read_tokens,
            COALESCE(SUM(reported_cache_read_input_tokens), 0)::bigint
                AS reported_total_read_tokens,
            COALESCE(MIN(reported_cache_creation_input_tokens), 0)::bigint
                AS reported_min_write_tokens,
            COALESCE(MAX(reported_cache_creation_input_tokens), 0)::bigint
                AS reported_max_write_tokens,
            COALESCE(AVG(reported_cache_creation_input_tokens), 0)::double precision
                AS reported_avg_write_tokens,
            COALESCE(SUM(reported_cache_creation_input_tokens), 0)::bigint
                AS reported_total_write_tokens,
            COUNT(*) FILTER (WHERE reported_cache_read_input_tokens >= warning_threshold)::bigint
                AS reported_read_warning_count,
            COUNT(*) FILTER (
                WHERE reported_cache_creation_input_tokens >= warning_threshold
            )::bigint AS reported_write_warning_count,
            COUNT(*) FILTER (
                WHERE reported_cache_read_input_tokens >= warning_threshold
                   OR reported_cache_creation_input_tokens >= warning_threshold
            )::bigint AS reported_either_warning_count,
            COUNT(*) FILTER (WHERE reported_cache_read_input_tokens >= critical_threshold)::bigint
                AS reported_read_critical_count,
            COUNT(*) FILTER (
                WHERE reported_cache_creation_input_tokens >= critical_threshold
            )::bigint AS reported_write_critical_count,
            COUNT(*) FILTER (
                WHERE reported_cache_read_input_tokens >= critical_threshold
                   OR reported_cache_creation_input_tokens >= critical_threshold
            )::bigint AS reported_either_critical_count,

            COALESCE(SUM(raw_cost_usd), 0)::double precision AS raw_cost_usd,
            COALESCE(SUM(reported_cost_usd), 0)::double precision AS reported_cost_usd,
            COALESCE(SUM(target_cost_usd), 0)::double precision AS target_cost_usd,
            COALESCE(SUM(reported_cost_usd - raw_cost_usd), 0)::double precision AS profit_usd,
            COALESCE(SUM(loss_usd), 0)::double precision AS total_loss_usd,
            COALESCE(SUM(target_gap_usd), 0)::double precision AS total_target_gap_usd,
            COALESCE(MAX(loss_usd), 0)::double precision AS max_loss_usd,
            COALESCE(MAX(target_gap_usd), 0)::double precision AS max_target_gap_usd,
            COALESCE(MAX(raw_cost_usd), 0)::double precision AS max_raw_cost_usd,
            COALESCE(MAX(reported_cost_usd), 0)::double precision AS max_reported_cost_usd,
            COUNT(*) FILTER (WHERE raw_cost_usd > 0 AND reported_cost_usd < raw_cost_usd)
                ::bigint AS below_raw_count,
            COUNT(*) FILTER (WHERE target_cost_usd > 0 AND reported_cost_usd < target_cost_usd)
                ::bigint AS below_target_count,
            COUNT(*) FILTER (WHERE cost_floor_applied)::bigint AS cost_floor_applied_records,
            MIN(cost_ratio) FILTER (WHERE cost_ratio IS NOT NULL) AS min_cost_ratio,
            AVG(cost_ratio) FILTER (WHERE cost_ratio IS NOT NULL) AS avg_cost_ratio,
            MAX(cost_ratio) FILTER (WHERE cost_ratio IS NOT NULL) AS max_cost_ratio
        FROM scored_records
        "#,
    );
    builder
}

fn external_pool_usage_risk_bucket_query<'a>(
    query: &'a UsageExternalPoolRiskQuery,
    cost_target_multiplier: f64,
) -> QueryBuilder<'a, Postgres> {
    let mut builder = QueryBuilder::<Postgres>::new("");
    push_external_pool_usage_risk_scored_cte(&mut builder, query, cost_target_multiplier);
    builder.push(
        r#"
        , buckets(key, label, min_tokens, max_tokens, ord) AS (
            VALUES
                ('zero', '0', 0::bigint, 0::bigint, 1),
                ('1_10k', '1-10K', 1::bigint, 10000::bigint, 2),
                ('10_100k', '10K-100K', 10001::bigint, 100000::bigint, 3),
                ('100_300k', '100K-300K', 100001::bigint, 300000::bigint, 4),
                ('300_600k', '300K-600K', 300001::bigint, 600000::bigint, 5),
                ('600_800k', '600K-800K', 600001::bigint, 799999::bigint, 6),
                ('800k_1m', '800K-1M', 800000::bigint, 999999::bigint, 7),
                ('gte_1m', '>=1M', 1000000::bigint, NULL::bigint, 8)
        )
        SELECT
            b.key,
            b.label,
            b.min_tokens,
            b.max_tokens,
            COUNT(*) FILTER (
                WHERE (b.max_tokens IS NULL AND raw_cache_read_input_tokens >= b.min_tokens)
                   OR (b.max_tokens IS NOT NULL
                       AND raw_cache_read_input_tokens >= b.min_tokens
                       AND raw_cache_read_input_tokens <= b.max_tokens)
            )::bigint AS raw_read_count,
            COUNT(*) FILTER (
                WHERE (b.max_tokens IS NULL AND raw_cache_creation_input_tokens >= b.min_tokens)
                   OR (b.max_tokens IS NOT NULL
                       AND raw_cache_creation_input_tokens >= b.min_tokens
                       AND raw_cache_creation_input_tokens <= b.max_tokens)
            )::bigint AS raw_write_count,
            COUNT(*) FILTER (
                WHERE (b.max_tokens IS NULL AND reported_cache_read_input_tokens >= b.min_tokens)
                   OR (b.max_tokens IS NOT NULL
                       AND reported_cache_read_input_tokens >= b.min_tokens
                       AND reported_cache_read_input_tokens <= b.max_tokens)
            )::bigint AS reported_read_count,
            COUNT(*) FILTER (
                WHERE (b.max_tokens IS NULL AND reported_cache_creation_input_tokens >= b.min_tokens)
                   OR (b.max_tokens IS NOT NULL
                       AND reported_cache_creation_input_tokens >= b.min_tokens
                       AND reported_cache_creation_input_tokens <= b.max_tokens)
            )::bigint AS reported_write_count
        FROM buckets b
        LEFT JOIN scored_records ON true
        GROUP BY b.key, b.label, b.min_tokens, b.max_tokens, b.ord
        ORDER BY b.ord
        "#,
    );
    builder
}

fn external_pool_usage_risk_group_query<'a>(
    query: &'a UsageExternalPoolRiskQuery,
    cost_target_multiplier: f64,
) -> QueryBuilder<'a, Postgres> {
    let mut builder = QueryBuilder::<Postgres>::new("");
    push_external_pool_usage_risk_scored_cte(&mut builder, query, cost_target_multiplier);
    builder.push(
        r#"
        , group_rows AS (
            SELECT
                'pool' AS group_kind,
                COALESCE(external_pool_id_text, 'unknown') AS group_key,
                COALESCE(
                    external_pool_name,
                    CASE
                        WHEN external_pool_id_text IS NOT NULL THEN '#' || external_pool_id_text
                        ELSE '未知外部池'
                    END
                ) AS group_label,
                status,
                has_warning_risk,
                has_critical_risk,
                reported_output_tokens,
                raw_cache_read_input_tokens,
                raw_cache_creation_input_tokens,
                reported_cache_read_input_tokens,
                reported_cache_creation_input_tokens,
                raw_cost_usd,
                reported_cost_usd,
                target_cost_usd,
                loss_usd,
                target_gap_usd
            FROM scored_records

            UNION ALL

            SELECT
                'path' AS group_kind,
                endpoint AS group_key,
                endpoint AS group_label,
                status,
                has_warning_risk,
                has_critical_risk,
                reported_output_tokens,
                raw_cache_read_input_tokens,
                raw_cache_creation_input_tokens,
                reported_cache_read_input_tokens,
                reported_cache_creation_input_tokens,
                raw_cost_usd,
                reported_cost_usd,
                target_cost_usd,
                loss_usd,
                target_gap_usd
            FROM scored_records

            UNION ALL

            SELECT
                'model' AS group_kind,
                COALESCE(pricing_model, model, 'unknown') AS group_key,
                COALESCE(pricing_model, model, 'unknown') AS group_label,
                status,
                has_warning_risk,
                has_critical_risk,
                reported_output_tokens,
                raw_cache_read_input_tokens,
                raw_cache_creation_input_tokens,
                reported_cache_read_input_tokens,
                reported_cache_creation_input_tokens,
                raw_cost_usd,
                reported_cost_usd,
                target_cost_usd,
                loss_usd,
                target_gap_usd
            FROM scored_records
        ), grouped AS (
            SELECT
                group_kind,
                group_key,
                group_label,
                COUNT(*)::bigint AS records,
                COUNT(*) FILTER (WHERE status = 'success')::bigint AS success_records,
                COUNT(*) FILTER (WHERE has_warning_risk)::bigint AS warning_records,
                COUNT(*) FILTER (WHERE has_critical_risk)::bigint AS critical_records,
                COUNT(*) FILTER (WHERE reported_output_tokens = 0)::bigint AS output_zero_records,
                COALESCE(MAX(raw_cache_read_input_tokens), 0)::bigint AS raw_read_max,
                COALESCE(MAX(raw_cache_creation_input_tokens), 0)::bigint AS raw_write_max,
                COALESCE(MAX(reported_cache_read_input_tokens), 0)::bigint AS reported_read_max,
                COALESCE(MAX(reported_cache_creation_input_tokens), 0)::bigint AS reported_write_max,
                COALESCE(SUM(raw_cost_usd), 0)::double precision AS raw_cost_usd,
                COALESCE(SUM(reported_cost_usd), 0)::double precision AS reported_cost_usd,
                COALESCE(SUM(target_cost_usd), 0)::double precision AS target_cost_usd,
                COALESCE(SUM(reported_cost_usd - raw_cost_usd), 0)::double precision AS profit_usd,
                COALESCE(SUM(loss_usd), 0)::double precision AS total_loss_usd,
                COALESCE(SUM(target_gap_usd), 0)::double precision AS total_target_gap_usd,
                COUNT(*) FILTER (WHERE raw_cost_usd > 0 AND reported_cost_usd < raw_cost_usd)
                    ::bigint AS below_raw_count,
                COUNT(*) FILTER (
                    WHERE target_cost_usd > 0 AND reported_cost_usd < target_cost_usd
                )::bigint AS below_target_count
            FROM group_rows
            GROUP BY group_kind, group_key, group_label
        ), ranked AS (
            SELECT
                *,
                row_number() OVER (
                    PARTITION BY group_kind
                    ORDER BY critical_records DESC,
                             warning_records DESC,
                             total_target_gap_usd DESC,
                             GREATEST(raw_read_max, raw_write_max, reported_read_max, reported_write_max) DESC,
                             records DESC,
                             group_key ASC
                ) AS rn
            FROM grouped
        )
        SELECT *
        FROM ranked
        WHERE rn <= 20
        ORDER BY group_kind, rn
        "#,
    );
    builder
}

fn external_pool_usage_risk_sample_query<'a>(
    query: &'a UsageExternalPoolRiskQuery,
    cost_target_multiplier: f64,
) -> QueryBuilder<'a, Postgres> {
    let mut builder = QueryBuilder::<Postgres>::new("");
    push_external_pool_usage_risk_scored_cte(&mut builder, query, cost_target_multiplier);
    builder.push(
        r#"
        SELECT
            id,
            created_at,
            endpoint,
            stream,
            model,
            status,
            external_pool_id_text,
            external_pool_name,
            pricing_model,
            usage_projection_mode,
            external_pool_billing_present,
            cost_floor_applied,
            raw_input_tokens,
            raw_output_tokens,
            raw_cache_read_input_tokens,
            raw_cache_creation_input_tokens,
            reported_input_tokens,
            reported_output_tokens,
            reported_cache_read_input_tokens,
            reported_cache_creation_input_tokens,
            raw_cost_usd,
            reported_cost_usd,
            target_cost_usd,
            loss_usd,
            target_gap_usd,
            cost_ratio,
            array_remove(ARRAY[
                CASE WHEN NOT external_pool_billing_present THEN 'missing_external_pool_billing' END,
                CASE WHEN reported_output_tokens = 0 THEN 'output_zero' END,
                CASE
                    WHEN raw_cache_read_input_tokens >= critical_threshold
                      OR raw_cache_creation_input_tokens >= critical_threshold
                    THEN 'raw_cache_critical'
                END,
                CASE
                    WHEN reported_cache_read_input_tokens >= critical_threshold
                      OR reported_cache_creation_input_tokens >= critical_threshold
                    THEN 'reported_cache_critical'
                END,
                CASE
                    WHEN raw_cache_read_input_tokens >= warning_threshold
                      OR raw_cache_creation_input_tokens >= warning_threshold
                    THEN 'raw_cache_warning'
                END,
                CASE
                    WHEN reported_cache_read_input_tokens >= warning_threshold
                      OR reported_cache_creation_input_tokens >= warning_threshold
                    THEN 'reported_cache_warning'
                END,
                CASE WHEN raw_cost_usd > 0 AND reported_cost_usd < raw_cost_usd THEN 'below_raw_cost' END,
                CASE
                    WHEN target_cost_usd > 0 AND reported_cost_usd < target_cost_usd
                    THEN 'below_target_cost'
                END
            ], NULL)::text[] AS risk_reasons
        FROM scored_records
        ORDER BY
            has_critical_risk DESC,
            has_warning_risk DESC,
            target_gap_usd DESC,
            loss_usd DESC,
            GREATEST(
                raw_cache_read_input_tokens,
                raw_cache_creation_input_tokens,
                reported_cache_read_input_tokens,
                reported_cache_creation_input_tokens
            ) DESC,
            created_at DESC,
            id DESC
        LIMIT "#,
    );
    builder.push_bind(usize_to_i64(query.limit));
    builder
}

fn push_external_pool_usage_risk_scored_cte<'a>(
    builder: &mut QueryBuilder<'a, Postgres>,
    query: &'a UsageExternalPoolRiskQuery,
    cost_target_multiplier: f64,
) {
    builder.push(
        r#"
        WITH risk_records AS (
            SELECT
                records.id,
                records.created_at,
                records.endpoint,
                records.stream,
                records.model,
                records.status,
                records.pricing_available,
                NULLIF(records.data->>'externalPoolId', '') AS external_pool_id_text,
                NULLIF(BTRIM(records.data->>'externalPoolName'), '') AS external_pool_name,
                COALESCE(
                    NULLIF(BTRIM(records.data #>> '{externalPoolBilling,pricingModel}'), ''),
                    NULLIF(BTRIM(records.pricing_model), '')
                ) AS pricing_model,
                NULLIF(BTRIM(records.data #>> '{externalPoolBilling,usageProjectionMode}'), '')
                    AS usage_projection_mode,
                COALESCE(jsonb_typeof(records.data->'externalPoolBilling') = 'object', false)
                    AS external_pool_billing_present,
                COALESCE(
                    jsonb_typeof(records.data #> '{externalPoolBilling,rawUsage}') = 'object',
                    false
                )
                    AS raw_usage_present,
                COALESCE(
                    jsonb_typeof(records.data #> '{externalPoolBilling,reportedUsage}') = 'object',
                    false
                )
                    AS reported_usage_present,
                COALESCE(records.data #> '{externalPoolBilling,costFloorApplied}' = 'true'::jsonb, false)
                    AS cost_floor_applied,
                CASE
                    WHEN jsonb_typeof(records.data #> '{externalPoolBilling,rawUsage,inputTokens}') = 'number'
                    THEN GREATEST(
                        LEAST(
                            (records.data #>> '{externalPoolBilling,rawUsage,inputTokens}')::numeric,
                            9223372036854775807::numeric
                        ),
                        0::numeric
                    )::bigint
                    ELSE 0::bigint
                END AS raw_input_tokens,
                CASE
                    WHEN jsonb_typeof(records.data #> '{externalPoolBilling,rawUsage,outputTokens}') = 'number'
                    THEN GREATEST(
                        LEAST(
                            (records.data #>> '{externalPoolBilling,rawUsage,outputTokens}')::numeric,
                            9223372036854775807::numeric
                        ),
                        0::numeric
                    )::bigint
                    ELSE 0::bigint
                END AS raw_output_tokens,
                CASE
                    WHEN jsonb_typeof(records.data #> '{externalPoolBilling,rawUsage,cacheReadInputTokens}') = 'number'
                    THEN GREATEST(
                        LEAST(
                            (records.data #>> '{externalPoolBilling,rawUsage,cacheReadInputTokens}')::numeric,
                            9223372036854775807::numeric
                        ),
                        0::numeric
                    )::bigint
                    ELSE 0::bigint
                END AS raw_cache_read_input_tokens,
                CASE
                    WHEN jsonb_typeof(records.data #> '{externalPoolBilling,rawUsage,cacheCreationInputTokens}') = 'number'
                    THEN GREATEST(
                        LEAST(
                            (records.data #>> '{externalPoolBilling,rawUsage,cacheCreationInputTokens}')::numeric,
                            9223372036854775807::numeric
                        ),
                        0::numeric
                    )::bigint
                    ELSE 0::bigint
                END AS raw_cache_creation_input_tokens,
                CASE
                    WHEN jsonb_typeof(records.data #> '{externalPoolBilling,reportedUsage,inputTokens}') = 'number'
                    THEN GREATEST(
                        LEAST(
                            (records.data #>> '{externalPoolBilling,reportedUsage,inputTokens}')::numeric,
                            9223372036854775807::numeric
                        ),
                        0::numeric
                    )::bigint
                    ELSE GREATEST(records.compat_input_tokens, 0)::bigint
                END AS reported_input_tokens,
                CASE
                    WHEN jsonb_typeof(records.data #> '{externalPoolBilling,reportedUsage,outputTokens}') = 'number'
                    THEN GREATEST(
                        LEAST(
                            (records.data #>> '{externalPoolBilling,reportedUsage,outputTokens}')::numeric,
                            9223372036854775807::numeric
                        ),
                        0::numeric
                    )::bigint
                    ELSE GREATEST(records.output_tokens, 0)::bigint
                END AS reported_output_tokens,
                CASE
                    WHEN jsonb_typeof(records.data #> '{externalPoolBilling,reportedUsage,cacheReadInputTokens}') = 'number'
                    THEN GREATEST(
                        LEAST(
                            (records.data #>> '{externalPoolBilling,reportedUsage,cacheReadInputTokens}')::numeric,
                            9223372036854775807::numeric
                        ),
                        0::numeric
                    )::bigint
                    ELSE GREATEST(records.cache_read_input_tokens, 0)::bigint
                END AS reported_cache_read_input_tokens,
                CASE
                    WHEN jsonb_typeof(records.data #> '{externalPoolBilling,reportedUsage,cacheCreationInputTokens}') = 'number'
                    THEN GREATEST(
                        LEAST(
                            (records.data #>> '{externalPoolBilling,reportedUsage,cacheCreationInputTokens}')::numeric,
                            9223372036854775807::numeric
                        ),
                        0::numeric
                    )::bigint
                    ELSE GREATEST(records.cache_creation_input_tokens, 0)::bigint
                END AS reported_cache_creation_input_tokens,
                CASE
                    WHEN jsonb_typeof(records.data #> '{externalPoolBilling,rawCostUsd}') = 'number'
                    THEN (records.data #>> '{externalPoolBilling,rawCostUsd}')::double precision
                    WHEN COALESCE(records.original_cost_usd, 0) <> 0
                    THEN records.original_cost_usd
                    ELSE 0::double precision
                END AS raw_cost_usd,
                CASE
                    WHEN jsonb_typeof(records.data #> '{externalPoolBilling,reportedCostUsd}') = 'number'
                    THEN (records.data #>> '{externalPoolBilling,reportedCostUsd}')::double precision
                    WHEN COALESCE(records.estimated_cost_usd, 0) <> 0
                    THEN records.estimated_cost_usd
                    ELSE 0::double precision
                END AS reported_cost_usd
            FROM usage_records records
            WHERE records.deleted_at IS NULL
              AND records.rollup_active
              AND records.created_at >= "#,
    );
    builder.push_bind(query.from);
    builder.push(" AND records.created_at < ");
    builder.push_bind(query.to);
    builder.push(
        r#"
              AND (
                  records.data->>'routeKind' = 'external_pool'
                  OR jsonb_typeof(records.data->'externalPoolBilling') = 'object'
              )
        "#,
    );

    if let Some(pool_id) = query.pool_id {
        builder.push(" AND records.data->>'externalPoolId' = ");
        builder.push_bind(pool_id.to_string());
    }
    if let Some(endpoint) = query.endpoint.as_deref() {
        builder.push(" AND records.endpoint = ");
        builder.push_bind(endpoint);
    }
    if let Some(model) = query.model.as_deref() {
        builder.push(
            r#"
              AND (
                  records.model = "#,
        );
        builder.push_bind(model);
        builder.push(" OR records.data->>'externalOutboundModel' = ");
        builder.push_bind(model);
        builder.push(" OR records.data #>> '{externalPoolBilling,pricingModel}' = ");
        builder.push_bind(model);
        builder.push(")");
    }
    if let Some(stream) = query.stream {
        builder.push(" AND records.stream = ");
        builder.push_bind(stream);
    }

    builder.push(
        r#"
        ), scored_records AS (
            SELECT
                risk_records.*,
        "#,
    );
    builder.push_bind(query.warning_threshold_tokens);
    builder.push("::bigint AS warning_threshold, ");
    builder.push_bind(query.critical_threshold_tokens);
    builder.push("::bigint AS critical_threshold, ");
    builder.push_bind(cost_target_multiplier);
    builder.push(
        r#"::double precision AS cost_target_multiplier,
                GREATEST(raw_cost_usd, 0::double precision)
                    * "#,
    );
    builder.push_bind(cost_target_multiplier);
    builder.push(
        r#"::double precision AS target_cost_usd,
                GREATEST(raw_cost_usd - reported_cost_usd, 0::double precision) AS loss_usd,
                GREATEST(
                    GREATEST(raw_cost_usd, 0::double precision)
                        * "#,
    );
    builder.push_bind(cost_target_multiplier);
    builder.push(
        r#"::double precision - reported_cost_usd,
                    0::double precision
                ) AS target_gap_usd,
                CASE
                    WHEN raw_cost_usd > 0 THEN reported_cost_usd / raw_cost_usd
                    ELSE NULL
                END AS cost_ratio,
                (
                    raw_cache_read_input_tokens >= "#,
    );
    builder.push_bind(query.warning_threshold_tokens);
    builder.push(" OR raw_cache_creation_input_tokens >= ");
    builder.push_bind(query.warning_threshold_tokens);
    builder.push(" OR reported_cache_read_input_tokens >= ");
    builder.push_bind(query.warning_threshold_tokens);
    builder.push(" OR reported_cache_creation_input_tokens >= ");
    builder.push_bind(query.warning_threshold_tokens);
    builder.push(
        r#"
                    OR (GREATEST(raw_cost_usd, 0::double precision) * "#,
    );
    builder.push_bind(cost_target_multiplier);
    builder.push(
        r#"::double precision) > reported_cost_usd
                    OR reported_output_tokens = 0
                    OR NOT external_pool_billing_present
                ) AS has_warning_risk,
                (
                    raw_cache_read_input_tokens >= "#,
    );
    builder.push_bind(query.critical_threshold_tokens);
    builder.push(" OR raw_cache_creation_input_tokens >= ");
    builder.push_bind(query.critical_threshold_tokens);
    builder.push(" OR reported_cache_read_input_tokens >= ");
    builder.push_bind(query.critical_threshold_tokens);
    builder.push(" OR reported_cache_creation_input_tokens >= ");
    builder.push_bind(query.critical_threshold_tokens);
    builder.push(
        r#"
                    OR (raw_cost_usd > 0 AND reported_cost_usd < raw_cost_usd)
                ) AS has_critical_risk
            FROM risk_records
        ) "#,
    );
}

fn external_pool_usage_risk_totals_from_row(
    row: &PgRow,
) -> anyhow::Result<UsageExternalPoolRiskTotals> {
    Ok(UsageExternalPoolRiskTotals {
        records: row_i64_to_usize(row, "records")?,
        success_records: row_i64_to_usize(row, "success_records")?,
        error_records: row_i64_to_usize(row, "error_records")?,
        stream_records: row_i64_to_usize(row, "stream_records")?,
        non_stream_records: row_i64_to_usize(row, "non_stream_records")?,
        priced_records: row_i64_to_usize(row, "priced_records")?,
        unpriced_records: row_i64_to_usize(row, "unpriced_records")?,
        raw_usage_records: row_i64_to_usize(row, "raw_usage_records")?,
        reported_usage_records: row_i64_to_usize(row, "reported_usage_records")?,
        missing_external_pool_billing_records: row_i64_to_usize(
            row,
            "missing_external_pool_billing_records",
        )?,
        output_zero_records: row_i64_to_usize(row, "output_zero_records")?,
    })
}

fn external_pool_usage_risk_cache_stats_from_row(
    row: &PgRow,
    prefix: &str,
) -> anyhow::Result<UsageExternalPoolRiskCacheStats> {
    Ok(UsageExternalPoolRiskCacheStats {
        min_read_tokens: row.try_get(format!("{}_min_read_tokens", prefix).as_str())?,
        max_read_tokens: row.try_get(format!("{}_max_read_tokens", prefix).as_str())?,
        avg_read_tokens: row.try_get(format!("{}_avg_read_tokens", prefix).as_str())?,
        total_read_tokens: row.try_get(format!("{}_total_read_tokens", prefix).as_str())?,
        min_write_tokens: row.try_get(format!("{}_min_write_tokens", prefix).as_str())?,
        max_write_tokens: row.try_get(format!("{}_max_write_tokens", prefix).as_str())?,
        avg_write_tokens: row.try_get(format!("{}_avg_write_tokens", prefix).as_str())?,
        total_write_tokens: row.try_get(format!("{}_total_write_tokens", prefix).as_str())?,
        read_warning_count: row_i64_to_usize(row, &format!("{}_read_warning_count", prefix))?,
        write_warning_count: row_i64_to_usize(row, &format!("{}_write_warning_count", prefix))?,
        either_warning_count: row_i64_to_usize(row, &format!("{}_either_warning_count", prefix))?,
        read_critical_count: row_i64_to_usize(row, &format!("{}_read_critical_count", prefix))?,
        write_critical_count: row_i64_to_usize(row, &format!("{}_write_critical_count", prefix))?,
        either_critical_count: row_i64_to_usize(row, &format!("{}_either_critical_count", prefix))?,
    })
}

fn external_pool_usage_risk_cost_stats_from_row(
    row: &PgRow,
) -> anyhow::Result<UsageExternalPoolRiskCostStats> {
    Ok(UsageExternalPoolRiskCostStats {
        raw_cost_usd: row.try_get("raw_cost_usd")?,
        reported_cost_usd: row.try_get("reported_cost_usd")?,
        target_cost_usd: row.try_get("target_cost_usd")?,
        profit_usd: row.try_get("profit_usd")?,
        total_loss_usd: row.try_get("total_loss_usd")?,
        total_target_gap_usd: row.try_get("total_target_gap_usd")?,
        max_loss_usd: row.try_get("max_loss_usd")?,
        max_target_gap_usd: row.try_get("max_target_gap_usd")?,
        max_raw_cost_usd: row.try_get("max_raw_cost_usd")?,
        max_reported_cost_usd: row.try_get("max_reported_cost_usd")?,
        below_raw_count: row_i64_to_usize(row, "below_raw_count")?,
        below_target_count: row_i64_to_usize(row, "below_target_count")?,
        cost_floor_applied_records: row_i64_to_usize(row, "cost_floor_applied_records")?,
        min_cost_ratio: row.try_get("min_cost_ratio")?,
        avg_cost_ratio: row.try_get("avg_cost_ratio")?,
        max_cost_ratio: row.try_get("max_cost_ratio")?,
    })
}

fn external_pool_usage_risk_bucket_from_row(
    row: PgRow,
) -> anyhow::Result<UsageExternalPoolRiskBucket> {
    Ok(UsageExternalPoolRiskBucket {
        key: row.try_get("key")?,
        label: row.try_get("label")?,
        min_tokens: row.try_get("min_tokens")?,
        max_tokens: row.try_get("max_tokens")?,
        raw_read_count: row_i64_to_usize(&row, "raw_read_count")?,
        raw_write_count: row_i64_to_usize(&row, "raw_write_count")?,
        reported_read_count: row_i64_to_usize(&row, "reported_read_count")?,
        reported_write_count: row_i64_to_usize(&row, "reported_write_count")?,
    })
}

fn external_pool_usage_risk_group_from_row(
    row: &PgRow,
) -> anyhow::Result<UsageExternalPoolRiskGroup> {
    Ok(UsageExternalPoolRiskGroup {
        key: row.try_get("group_key")?,
        label: row.try_get("group_label")?,
        records: row_i64_to_usize(row, "records")?,
        success_records: row_i64_to_usize(row, "success_records")?,
        warning_records: row_i64_to_usize(row, "warning_records")?,
        critical_records: row_i64_to_usize(row, "critical_records")?,
        output_zero_records: row_i64_to_usize(row, "output_zero_records")?,
        raw_read_max: row.try_get("raw_read_max")?,
        raw_write_max: row.try_get("raw_write_max")?,
        reported_read_max: row.try_get("reported_read_max")?,
        reported_write_max: row.try_get("reported_write_max")?,
        raw_cost_usd: row.try_get("raw_cost_usd")?,
        reported_cost_usd: row.try_get("reported_cost_usd")?,
        target_cost_usd: row.try_get("target_cost_usd")?,
        profit_usd: row.try_get("profit_usd")?,
        total_loss_usd: row.try_get("total_loss_usd")?,
        total_target_gap_usd: row.try_get("total_target_gap_usd")?,
        below_raw_count: row_i64_to_usize(row, "below_raw_count")?,
        below_target_count: row_i64_to_usize(row, "below_target_count")?,
    })
}

fn external_pool_usage_risk_sample_from_row(
    row: PgRow,
) -> anyhow::Result<UsageExternalPoolRiskSample> {
    let created_at: DateTime<Utc> = row.try_get("created_at")?;
    Ok(UsageExternalPoolRiskSample {
        id: row.try_get("id")?,
        created_at: created_at.to_rfc3339(),
        endpoint: row.try_get("endpoint")?,
        stream: row.try_get("stream")?,
        model: row.try_get("model")?,
        status: row.try_get("status")?,
        external_pool_id: row
            .try_get::<Option<String>, _>("external_pool_id_text")?
            .and_then(|value| value.parse::<u64>().ok()),
        external_pool_name: row.try_get("external_pool_name")?,
        pricing_model: row.try_get("pricing_model")?,
        usage_projection_mode: row.try_get("usage_projection_mode")?,
        external_pool_billing_present: row.try_get("external_pool_billing_present")?,
        cost_floor_applied: row.try_get("cost_floor_applied")?,
        raw_input_tokens: row.try_get("raw_input_tokens")?,
        raw_output_tokens: row.try_get("raw_output_tokens")?,
        raw_cache_read_input_tokens: row.try_get("raw_cache_read_input_tokens")?,
        raw_cache_creation_input_tokens: row.try_get("raw_cache_creation_input_tokens")?,
        reported_input_tokens: row.try_get("reported_input_tokens")?,
        reported_output_tokens: row.try_get("reported_output_tokens")?,
        reported_cache_read_input_tokens: row.try_get("reported_cache_read_input_tokens")?,
        reported_cache_creation_input_tokens: row
            .try_get("reported_cache_creation_input_tokens")?,
        raw_cost_usd: row.try_get("raw_cost_usd")?,
        reported_cost_usd: row.try_get("reported_cost_usd")?,
        target_cost_usd: row.try_get("target_cost_usd")?,
        loss_usd: row.try_get("loss_usd")?,
        target_gap_usd: row.try_get("target_gap_usd")?,
        cost_ratio: row.try_get("cost_ratio")?,
        risk_reasons: row.try_get("risk_reasons")?,
    })
}

fn apply_usage_record_legacy_cost_compatibility(record: &mut UsageRecord) {
    if record.original_cost_usd != 0.0 {
        return;
    }

    if let Some(external_pool_billing) = record
        .external_pool_billing
        .as_ref()
        .filter(|billing| billing.raw_cost_usd != 0.0)
    {
        record.original_cost_usd = external_pool_billing.raw_cost_usd;
        return;
    }

    if record.estimated_cost_usd != 0.0 {
        record.original_cost_usd = record.estimated_cost_usd;
    }
}

async fn upsert_usage_record_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    record: &UsageRecord,
) -> anyhow::Result<bool> {
    let value = serde_json::to_value(record)?;
    let created_at = chrono::DateTime::parse_from_rfc3339(&record.created_at)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());
    let applied = sqlx::query(
        r#"
        INSERT INTO usage_records (
            id, created_at, endpoint, stream, model, conversation_id, credential_id,
            credential_label, status, usage_source, total_input_tokens, compat_input_tokens,
            billable_input_tokens, output_tokens, cache_read_input_tokens,
            cache_creation_input_tokens, cache_creation_5m_input_tokens,
            cache_creation_1h_input_tokens, estimated_cost_usd, original_cost_usd, kiro_metering_usage,
            pricing_available, pricing_model, duration_ms, simulated, sticky_bound, fallback_from_sticky,
            error_type, error_message, error_detail, data
        )
        SELECT
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
            $11, $12, $13, $14, $15, $16, $17, $18,
            $19, $20, $21, $22, $23, $24, $25, $26, $27, $28, $29, $30, $31
        WHERE $2 >= COALESCE(
            (
                SELECT cutoff_at
                FROM usage_cleanup_watermarks
                WHERE scope = 'soft_delete_created_at'
            ),
            '-infinity'::timestamptz
        )
        ON CONFLICT (id) DO UPDATE
        SET created_at = EXCLUDED.created_at,
            endpoint = EXCLUDED.endpoint,
            stream = EXCLUDED.stream,
            model = EXCLUDED.model,
            conversation_id = EXCLUDED.conversation_id,
            credential_id = EXCLUDED.credential_id,
            credential_label = EXCLUDED.credential_label,
            status = EXCLUDED.status,
            usage_source = EXCLUDED.usage_source,
            total_input_tokens = EXCLUDED.total_input_tokens,
            compat_input_tokens = EXCLUDED.compat_input_tokens,
            billable_input_tokens = EXCLUDED.billable_input_tokens,
            output_tokens = EXCLUDED.output_tokens,
            cache_read_input_tokens = EXCLUDED.cache_read_input_tokens,
            cache_creation_input_tokens = EXCLUDED.cache_creation_input_tokens,
            cache_creation_5m_input_tokens = EXCLUDED.cache_creation_5m_input_tokens,
            cache_creation_1h_input_tokens = EXCLUDED.cache_creation_1h_input_tokens,
            estimated_cost_usd = EXCLUDED.estimated_cost_usd,
            original_cost_usd = EXCLUDED.original_cost_usd,
            kiro_metering_usage = EXCLUDED.kiro_metering_usage,
            pricing_available = EXCLUDED.pricing_available,
            pricing_model = EXCLUDED.pricing_model,
            duration_ms = EXCLUDED.duration_ms,
            simulated = EXCLUDED.simulated,
            sticky_bound = EXCLUDED.sticky_bound,
            fallback_from_sticky = EXCLUDED.fallback_from_sticky,
            error_type = EXCLUDED.error_type,
            error_message = EXCLUDED.error_message,
            error_detail = EXCLUDED.error_detail,
            data = EXCLUDED.data,
            updated_at = now()
        WHERE usage_records.deleted_at IS NULL
          AND usage_records.rollup_active
        RETURNING id
        "#,
    )
    .bind(&record.id)
    .bind(created_at)
    .bind(&record.endpoint)
    .bind(record.stream)
    .bind(&record.model)
    .bind(&record.conversation_id)
    .bind(record.credential_id.map(|id| id as i64))
    .bind(&record.credential_label)
    .bind(usage_status_value(record.status))
    .bind(usage_source_value(record.usage_source))
    .bind(record.total_input_tokens)
    .bind(record.compat_input_tokens)
    .bind(record.billable_input_tokens)
    .bind(record.output_tokens)
    .bind(record.cache_read_input_tokens)
    .bind(record.cache_creation_input_tokens)
    .bind(record.cache_creation_5m_input_tokens)
    .bind(record.cache_creation_1h_input_tokens)
    .bind(record.estimated_cost_usd)
    .bind(record.original_cost_usd)
    .bind(record.kiro_metering_usage)
    .bind(record.pricing_available)
    .bind(&record.pricing_model)
    .bind(u64_to_i64(record.duration_ms))
    .bind(record.simulated)
    .bind(record.sticky_bound)
    .bind(record.fallback_from_sticky)
    .bind(&record.error_type)
    .bind(&record.error_message)
    .bind(&record.error_detail)
    .bind(value)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(applied.is_some())
}

fn cleanup_preview_from_row(row: PgRow) -> anyhow::Result<UsageCleanupPreview> {
    let matched_rows = row_i64_to_u64(&row, "matched_rows")?;
    Ok(UsageCleanupPreview {
        matched_rows,
        oldest_created_at: row.try_get("oldest_created_at")?,
        newest_created_at: row.try_get("newest_created_at")?,
    })
}

async fn configure_usage_cleanup_transaction(
    tx: &mut Transaction<'_, Postgres>,
) -> anyhow::Result<()> {
    sqlx::query(&format!(
        "SET LOCAL lock_timeout = '{}ms'",
        USAGE_CLEANUP_LOCK_TIMEOUT_MS
    ))
    .execute(&mut **tx)
    .await?;
    sqlx::query(&format!(
        "SET LOCAL statement_timeout = '{}ms'",
        USAGE_CLEANUP_STATEMENT_TIMEOUT_MS
    ))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn configure_usage_dashboard_read_transaction(
    tx: &mut Transaction<'_, Postgres>,
) -> anyhow::Result<()> {
    sqlx::query("SET TRANSACTION READ ONLY")
        .execute(&mut **tx)
        .await?;
    sqlx::query(&format!(
        "SET LOCAL statement_timeout = '{}ms'",
        USAGE_DASHBOARD_STATEMENT_TIMEOUT_MS
    ))
    .execute(&mut **tx)
    .await?;
    sqlx::query(&format!(
        "SET LOCAL lock_timeout = '{}ms'",
        USAGE_DASHBOARD_LOCK_TIMEOUT_MS
    ))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn acquire_usage_writer_commit_guard(
    tx: &mut Transaction<'_, Postgres>,
) -> anyhow::Result<()> {
    let lock_id = usage_cleanup_commit_lock_id(tx).await?;
    sqlx::query("SELECT pg_advisory_xact_lock_shared($1)")
        .bind(lock_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

#[cfg(test)]
async fn usage_advisory_lock_scope(tx: &mut Transaction<'_, Postgres>) -> anyhow::Result<i64> {
    let schema: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(&mut **tx)
        .await?;
    let digest = Sha256::digest(schema.as_bytes());
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    Ok(i64::from_be_bytes(bytes))
}

#[cfg(not(test))]
async fn usage_advisory_lock_scope(_tx: &mut Transaction<'_, Postgres>) -> anyhow::Result<i64> {
    Ok(0)
}

async fn usage_cleanup_commit_lock_id(tx: &mut Transaction<'_, Postgres>) -> anyhow::Result<i64> {
    Ok(USAGE_CLEANUP_COMMIT_LOCK_ID ^ usage_advisory_lock_scope(tx).await?)
}

fn usage_record_commit_lock_id(id: &str, scope: i64) -> i64 {
    let digest = Sha256::digest(id.as_bytes());
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    i64::from_be_bytes(bytes) ^ USAGE_RECORD_COMMIT_LOCK_DOMAIN ^ scope
}

async fn acquire_usage_record_commit_guards(
    tx: &mut Transaction<'_, Postgres>,
    ids: &[String],
) -> anyhow::Result<()> {
    let scope = usage_advisory_lock_scope(tx).await?;
    let mut lock_ids = ids
        .iter()
        .map(|id| usage_record_commit_lock_id(id, scope))
        .collect::<Vec<_>>();
    lock_ids.sort_unstable();
    lock_ids.dedup();
    sqlx::query(
        r#"
        SELECT pg_advisory_xact_lock(lock_id)
        FROM unnest($1::bigint[]) AS locks(lock_id)
        ORDER BY lock_id
        "#,
    )
    .bind(&lock_ids)
    .fetch_all(&mut **tx)
    .await?;
    Ok(())
}

async fn acquire_usage_cleanup_commit_guard(
    tx: &mut Transaction<'_, Postgres>,
) -> anyhow::Result<()> {
    let lock_id = usage_cleanup_commit_lock_id(tx).await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(lock_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn try_acquire_usage_cleanup_commit_guard(
    tx: &mut Transaction<'_, Postgres>,
) -> anyhow::Result<bool> {
    let lock_id = usage_cleanup_commit_lock_id(tx).await?;
    sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
        .bind(lock_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(Into::into)
}

fn is_postgres_cleanup_retryable_timeout_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        let message = cause.to_string();
        let is_retryable_code = cause
            .downcast_ref::<sqlx::Error>()
            .and_then(sqlx::Error::as_database_error)
            .and_then(|database_error| database_error.code())
            .is_some_and(|code| matches!(code.as_ref(), "55P03" | "57014"));
        is_retryable_code
            || message.contains("lock timeout")
            || message.contains("canceling statement due to lock timeout")
            || message.contains("statement timeout")
            || message.contains("canceling statement due to statement timeout")
    })
}

fn usage_cleanup_job_from_row(row: PgRow) -> anyhow::Result<UsageCleanupJobRow> {
    let optional_u64 = |column: &str| -> anyhow::Result<Option<u64>> {
        let value: Option<i64> = row.try_get(column)?;
        Ok(value.map(|value| value.max(0) as u64))
    };
    let nonnegative_int = |column: &str| -> anyhow::Result<usize> {
        let value: i32 = row.try_get(column)?;
        Ok(value.max(0) as usize)
    };
    Ok(UsageCleanupJobRow {
        job_id: row.try_get("job_id")?,
        mode: row.try_get("mode")?,
        cutoff_at: row.try_get("cutoff_at")?,
        batch_size: nonnegative_int("batch_size")?,
        max_batches: nonnegative_int("max_batches")?,
        pause_ms_between_batches: row_i64_to_u64(&row, "pause_ms_between_batches")?,
        status: row.try_get("status")?,
        phase: row.try_get("phase")?,
        matched_rows: optional_u64("matched_rows")?,
        remaining_rows: optional_u64("remaining_rows")?,
        processed_rows: row_i64_to_u64(&row, "processed_rows")?,
        last_batch_rows: row_i64_to_u64(&row, "last_batch_rows")?,
        batches: nonnegative_int("batches")?,
        cancel_requested: row.try_get("cancel_requested")?,
        stop_reason: row.try_get("stop_reason")?,
        last_error: row.try_get("last_error")?,
        redis_deleted_keys: row_i64_to_u64(&row, "redis_deleted_keys")? as usize,
        redis_delete_commands: row_i64_to_u64(&row, "redis_delete_commands")? as usize,
        redis_max_command_keys: nonnegative_int("redis_max_command_keys")?,
        redis_scan_passes: nonnegative_int("redis_scan_passes")?,
        redis_used_del_fallback: row.try_get("redis_used_del_fallback")?,
        redis_pass_limit_reached: row.try_get("redis_pass_limit_reached")?,
        started_at: row.try_get("started_at")?,
        updated_at: row.try_get("updated_at")?,
        finished_at: row.try_get("finished_at")?,
        lease_until: row.try_get("lease_until")?,
    })
}

#[derive(Clone, Copy, Default)]
struct UsageRollupMetrics {
    requests: i64,
    success_requests: i64,
    error_requests: i64,
    stream_requests: i64,
    non_stream_requests: i64,
    priced_requests: i64,
    unpriced_requests: i64,
    local_prompt_cache_requests: i64,
    simulated_requests: i64,
    upstream_metadata_requests: i64,
    sticky_bound_requests: i64,
    fallback_from_sticky_requests: i64,
    total_input_tokens: i64,
    billable_input_tokens: i64,
    total_output_tokens: i64,
    total_cache_read_input_tokens: i64,
    total_cache_creation_input_tokens: i64,
    local_prompt_cache_input_tokens: i64,
    local_prompt_cache_read_input_tokens: i64,
    local_prompt_cache_creation_input_tokens: i64,
    total_estimated_cost_usd: f64,
    total_original_cost_usd: f64,
    total_kiro_metering_usage: f64,
    external_pool_requests: i64,
    external_pool_priced_requests: i64,
    external_pool_unpriced_requests: i64,
    external_pool_cost_floor_applied_requests: i64,
    external_pool_raw_cost_usd: f64,
    external_pool_shaped_cost_usd: f64,
    external_pool_uplifted_cost_usd: f64,
    external_pool_profit_usd: f64,
    external_pool_reported_cost_usd: f64,
    external_pool_billable_cost_usd: f64,
    external_pool_cost_floor_delta_usd: f64,
    duration_ms_sum: i64,
    duration_ms_count: i64,
    duration_ms_max: i64,
}

impl UsageRollupMetrics {
    fn from_record(record: &UsageRecord, direction: i64) -> Self {
        let sign = if direction < 0 { -1 } else { 1 };
        let success = record.status == UsageRecordStatus::Success;
        let local_prompt_cache = record.usage_source == UsageSource::LocalPromptCache;
        let upstream_metadata = record.usage_source == UsageSource::UpstreamMetadata;
        let external_pool = record.route_kind == Some(UsageRouteKind::ExternalPool);
        let external_billing = record.external_pool_billing.as_ref();
        let external_priced =
            external_pool && external_billing.is_some_and(|billing| billing.pricing_available);
        Self {
            requests: sign,
            success_requests: signed_bool(success, sign),
            error_requests: signed_bool(!success, sign),
            stream_requests: signed_bool(record.stream, sign),
            non_stream_requests: signed_bool(!record.stream, sign),
            priced_requests: signed_bool(record.pricing_available, sign),
            unpriced_requests: signed_bool(!record.pricing_available, sign),
            local_prompt_cache_requests: signed_bool(local_prompt_cache, sign),
            simulated_requests: signed_bool(record.simulated, sign),
            upstream_metadata_requests: signed_bool(upstream_metadata, sign),
            sticky_bound_requests: signed_bool(record.sticky_bound, sign),
            fallback_from_sticky_requests: signed_bool(record.fallback_from_sticky, sign),
            total_input_tokens: signed_i32(record.total_input_tokens, sign),
            billable_input_tokens: signed_i32(record.billable_input_tokens, sign),
            total_output_tokens: signed_i32(record.output_tokens, sign),
            total_cache_read_input_tokens: signed_i32(record.cache_read_input_tokens, sign),
            total_cache_creation_input_tokens: signed_i32(record.cache_creation_input_tokens, sign),
            local_prompt_cache_input_tokens: if local_prompt_cache {
                signed_i32(record.total_input_tokens, sign)
            } else {
                0
            },
            local_prompt_cache_read_input_tokens: if local_prompt_cache {
                signed_i32(record.cache_read_input_tokens, sign)
            } else {
                0
            },
            local_prompt_cache_creation_input_tokens: if local_prompt_cache {
                signed_i32(record.cache_creation_input_tokens, sign)
            } else {
                0
            },
            total_estimated_cost_usd: record.estimated_cost_usd * sign as f64,
            total_original_cost_usd: record.original_cost_usd * sign as f64,
            total_kiro_metering_usage: record.kiro_metering_usage * sign as f64,
            external_pool_requests: signed_bool(external_pool, sign),
            external_pool_priced_requests: signed_bool(external_priced, sign),
            external_pool_unpriced_requests: signed_bool(external_pool && !external_priced, sign),
            external_pool_cost_floor_applied_requests: signed_bool(
                external_billing.is_some_and(|billing| billing.cost_floor_applied),
                sign,
            ),
            external_pool_raw_cost_usd: external_billing
                .map(|billing| billing.raw_cost_usd * sign as f64)
                .unwrap_or(0.0),
            external_pool_shaped_cost_usd: external_billing
                .map(|billing| billing.effective_shaped_cost_usd() * sign as f64)
                .unwrap_or(0.0),
            external_pool_uplifted_cost_usd: external_billing
                .map(|billing| billing.effective_uplifted_cost_usd() * sign as f64)
                .unwrap_or(0.0),
            external_pool_profit_usd: external_billing
                .map(|billing| billing.effective_profit_usd() * sign as f64)
                .unwrap_or(0.0),
            external_pool_reported_cost_usd: external_billing
                .map(|billing| billing.reported_cost_usd * sign as f64)
                .unwrap_or(0.0),
            external_pool_billable_cost_usd: external_billing
                .map(|billing| billing.billable_cost_usd * sign as f64)
                .unwrap_or(0.0),
            external_pool_cost_floor_delta_usd: external_billing
                .map(|billing| billing.cost_floor_delta_usd * sign as f64)
                .unwrap_or(0.0),
            duration_ms_sum: (record.duration_ms.min(i64::MAX as u64) as i64) * sign,
            duration_ms_count: sign,
            duration_ms_max: if sign > 0 {
                record.duration_ms.min(i64::MAX as u64) as i64
            } else {
                0
            },
        }
    }

    fn add(&mut self, other: Self) {
        self.requests += other.requests;
        self.success_requests += other.success_requests;
        self.error_requests += other.error_requests;
        self.stream_requests += other.stream_requests;
        self.non_stream_requests += other.non_stream_requests;
        self.priced_requests += other.priced_requests;
        self.unpriced_requests += other.unpriced_requests;
        self.local_prompt_cache_requests += other.local_prompt_cache_requests;
        self.simulated_requests += other.simulated_requests;
        self.upstream_metadata_requests += other.upstream_metadata_requests;
        self.sticky_bound_requests += other.sticky_bound_requests;
        self.fallback_from_sticky_requests += other.fallback_from_sticky_requests;
        self.total_input_tokens += other.total_input_tokens;
        self.billable_input_tokens += other.billable_input_tokens;
        self.total_output_tokens += other.total_output_tokens;
        self.total_cache_read_input_tokens += other.total_cache_read_input_tokens;
        self.total_cache_creation_input_tokens += other.total_cache_creation_input_tokens;
        self.local_prompt_cache_input_tokens += other.local_prompt_cache_input_tokens;
        self.local_prompt_cache_read_input_tokens += other.local_prompt_cache_read_input_tokens;
        self.local_prompt_cache_creation_input_tokens +=
            other.local_prompt_cache_creation_input_tokens;
        self.total_estimated_cost_usd += other.total_estimated_cost_usd;
        self.total_original_cost_usd += other.total_original_cost_usd;
        self.total_kiro_metering_usage += other.total_kiro_metering_usage;
        self.external_pool_requests += other.external_pool_requests;
        self.external_pool_priced_requests += other.external_pool_priced_requests;
        self.external_pool_unpriced_requests += other.external_pool_unpriced_requests;
        self.external_pool_cost_floor_applied_requests +=
            other.external_pool_cost_floor_applied_requests;
        self.external_pool_raw_cost_usd += other.external_pool_raw_cost_usd;
        self.external_pool_shaped_cost_usd += other.external_pool_shaped_cost_usd;
        self.external_pool_uplifted_cost_usd += other.external_pool_uplifted_cost_usd;
        self.external_pool_profit_usd += other.external_pool_profit_usd;
        self.external_pool_reported_cost_usd += other.external_pool_reported_cost_usd;
        self.external_pool_billable_cost_usd += other.external_pool_billable_cost_usd;
        self.external_pool_cost_floor_delta_usd += other.external_pool_cost_floor_delta_usd;
        self.duration_ms_sum += other.duration_ms_sum;
        self.duration_ms_count += other.duration_ms_count;
        self.duration_ms_max = self.duration_ms_max.max(other.duration_ms_max);
    }
}

struct UsageRollupDimension {
    dimension: &'static str,
    key: String,
    label: Option<String>,
    include_time_bucket: bool,
}

#[derive(Default)]
struct UsageRollupAggregate {
    label: Option<String>,
    metrics: UsageRollupMetrics,
}

impl UsageRollupAggregate {
    fn add(&mut self, label: Option<String>, metrics: UsageRollupMetrics) {
        if label.is_some() {
            self.label = label;
        }
        self.metrics.add(metrics);
    }
}

#[derive(Default)]
struct CredentialUsageSummaryDelta {
    requests: i64,
    estimated_cost_usd: f64,
    original_cost_usd: f64,
    kiro_metering_usage: f64,
    priced_requests: i64,
    unpriced_requests: i64,
}

#[derive(Default)]
struct UsageRollupBatchDelta {
    totals: HashMap<(&'static str, String), UsageRollupAggregate>,
    time_buckets: HashMap<(DateTime<Utc>, &'static str, String), UsageRollupAggregate>,
    cache_read_totals: HashMap<i32, i64>,
    cache_read_time_buckets: HashMap<(DateTime<Utc>, i32), i64>,
    duration_time_buckets: HashMap<(DateTime<Utc>, i32), i64>,
    credential_summaries: HashMap<u64, CredentialUsageSummaryDelta>,
    has_negative: bool,
}

impl UsageRollupBatchDelta {
    fn add_record(&mut self, record: &UsageRecord, direction: i64) {
        let direction = if direction < 0 { -1 } else { 1 };
        self.has_negative |= direction < 0;
        let metrics = UsageRollupMetrics::from_record(record, direction);
        let created_at = parse_usage_created_at(record);
        let bucket_start = usage_rollup_bucket_start(created_at);
        for dimension in usage_rollup_dimensions(record) {
            self.totals
                .entry((dimension.dimension, dimension.key.clone()))
                .or_default()
                .add(dimension.label.clone(), metrics);
            if dimension.include_time_bucket {
                self.time_buckets
                    .entry((bucket_start, dimension.dimension, dimension.key))
                    .or_default()
                    .add(dimension.label, metrics);
            }
        }

        let cache_read = record.cache_read_input_tokens.max(0);
        *self.cache_read_totals.entry(cache_read).or_default() += direction;
        *self
            .cache_read_time_buckets
            .entry((bucket_start, cache_read))
            .or_default() += direction;
        *self
            .duration_time_buckets
            .entry((bucket_start, record.duration_ms.min(i32::MAX as u64) as i32))
            .or_default() += direction;

        if let Some(credential_id) = record.credential_id {
            let summary = self.credential_summaries.entry(credential_id).or_default();
            summary.requests += direction;
            summary.estimated_cost_usd += record.estimated_cost_usd * direction as f64;
            summary.original_cost_usd += record.original_cost_usd * direction as f64;
            summary.kiro_metering_usage += record.kiro_metering_usage * direction as f64;
            summary.priced_requests += signed_bool(record.pricing_available, direction);
            summary.unpriced_requests += signed_bool(!record.pricing_available, direction);
        }
    }

    async fn apply(self, tx: &mut Transaction<'_, Postgres>) -> anyhow::Result<()> {
        let UsageRollupBatchDelta {
            totals,
            time_buckets,
            cache_read_totals,
            cache_read_time_buckets,
            duration_time_buckets,
            credential_summaries,
            has_negative,
        } = self;

        // Every writer touches shared global/status/model rollup rows. HashMap
        // iteration order differs across processes, so using it directly creates
        // a cross-instance row-lock inversion even for one-record batches.
        let mut totals = totals.into_iter().collect::<Vec<_>>();
        totals.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
        let mut time_buckets = time_buckets.into_iter().collect::<Vec<_>>();
        time_buckets.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
        let mut cache_read_totals = cache_read_totals.into_iter().collect::<Vec<_>>();
        cache_read_totals.sort_unstable_by_key(|(cache_read, _)| *cache_read);
        let mut cache_read_time_buckets = cache_read_time_buckets.into_iter().collect::<Vec<_>>();
        cache_read_time_buckets.sort_unstable_by_key(|(key, _)| *key);
        let mut duration_time_buckets = duration_time_buckets.into_iter().collect::<Vec<_>>();
        duration_time_buckets.sort_unstable_by_key(|(key, _)| *key);
        let mut credential_summaries = credential_summaries.into_iter().collect::<Vec<_>>();
        credential_summaries.sort_unstable_by_key(|(credential_id, _)| *credential_id);

        let (affected_total_dimensions, affected_total_keys): (Vec<_>, Vec<_>) = totals
            .iter()
            .map(|((dimension, key), _)| ((*dimension).to_string(), key.clone()))
            .unzip();
        let (affected_time_buckets, affected_time_dimensions, affected_time_keys) = time_buckets
            .iter()
            .fold((Vec::new(), Vec::new(), Vec::new()), |mut out, entry| {
                out.0.push((entry.0).0);
                out.1.push((entry.0).1.to_string());
                out.2.push((entry.0).2.clone());
                out
            });
        let affected_credentials = credential_summaries
            .iter()
            .map(|(credential_id, _)| *credential_id as i64)
            .collect::<Vec<_>>();
        let affected_cache_reads = cache_read_totals
            .iter()
            .map(|(cache_read, _)| *cache_read)
            .collect::<Vec<_>>();
        let mut affected_cache_buckets = cache_read_time_buckets
            .iter()
            .map(|((bucket_start, _), _)| *bucket_start)
            .collect::<Vec<_>>();
        affected_cache_buckets.sort_unstable();
        affected_cache_buckets.dedup();
        let mut affected_duration_buckets = duration_time_buckets
            .iter()
            .map(|((bucket_start, _), _)| *bucket_start)
            .collect::<Vec<_>>();
        affected_duration_buckets.sort_unstable();
        affected_duration_buckets.dedup();
        for ((dimension, key), aggregate) in totals {
            let dimension = UsageRollupDimension {
                dimension,
                key,
                label: aggregate.label,
                include_time_bucket: false,
            };
            upsert_usage_rollup_total(tx, &dimension, aggregate.metrics).await?;
        }
        for ((bucket_start, dimension, key), aggregate) in time_buckets {
            let dimension = UsageRollupDimension {
                dimension,
                key,
                label: aggregate.label,
                include_time_bucket: true,
            };
            upsert_usage_rollup_time_bucket(tx, bucket_start, &dimension, aggregate.metrics)
                .await?;
        }
        for (cache_read, requests) in cache_read_totals {
            upsert_usage_cache_read_total(tx, cache_read, requests).await?;
        }
        for ((bucket_start, cache_read), requests) in cache_read_time_buckets {
            upsert_usage_cache_read_time_bucket(tx, bucket_start, cache_read, requests).await?;
        }
        for ((bucket_start, duration_ms), requests) in duration_time_buckets {
            upsert_usage_duration_time_bucket(tx, bucket_start, duration_ms, requests).await?;
        }
        for (credential_id, delta) in credential_summaries {
            upsert_credential_usage_summary_delta(tx, credential_id, delta).await?;
        }
        if has_negative {
            reconcile_usage_rollups_after_negative_delta(
                tx,
                &affected_total_dimensions,
                &affected_total_keys,
                &affected_time_buckets,
                &affected_time_dimensions,
                &affected_time_keys,
                &affected_credentials,
                &affected_cache_reads,
                &affected_cache_buckets,
                &affected_duration_buckets,
            )
            .await?;
        }
        Ok(())
    }
}

fn usage_rollup_dimensions(record: &UsageRecord) -> Vec<UsageRollupDimension> {
    let status = usage_status_value(record.status).to_string();
    let source = usage_source_value(record.usage_source).to_string();
    let model = non_empty_or_unknown(&record.model);
    let endpoint = non_empty_or_unknown(&record.endpoint);
    let mut dimensions = vec![
        UsageRollupDimension {
            dimension: "global",
            key: "all".to_string(),
            label: None,
            include_time_bucket: true,
        },
        UsageRollupDimension {
            dimension: "status",
            key: status,
            label: None,
            include_time_bucket: true,
        },
        UsageRollupDimension {
            dimension: "usage_source",
            key: source,
            label: None,
            include_time_bucket: true,
        },
        UsageRollupDimension {
            dimension: "model",
            key: model,
            label: None,
            include_time_bucket: true,
        },
        UsageRollupDimension {
            dimension: "endpoint",
            key: endpoint,
            label: None,
            include_time_bucket: true,
        },
    ];

    if let Some(credential_id) = record.credential_id {
        dimensions.push(UsageRollupDimension {
            dimension: "credential",
            key: credential_id.to_string(),
            label: record
                .credential_label
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            include_time_bucket: true,
        });
    }

    if let Some(external_pool_id) = record.external_pool_id {
        dimensions.push(UsageRollupDimension {
            dimension: "external_pool",
            key: external_pool_id.to_string(),
            label: record
                .external_pool_name
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            include_time_bucket: true,
        });
    }

    if let Some(conversation_id) = record
        .conversation_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        dimensions.push(UsageRollupDimension {
            dimension: "conversation",
            key: conversation_id.to_string(),
            label: None,
            include_time_bucket: false,
        });
    }

    if record.status != UsageRecordStatus::Success {
        dimensions.push(UsageRollupDimension {
            dimension: "error",
            key: record
                .error_type
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(usage_status_value(record.status))
                .to_string(),
            label: record
                .error_message
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            include_time_bucket: true,
        });
    }

    dimensions
}

async fn upsert_usage_rollup_total(
    tx: &mut Transaction<'_, Postgres>,
    dimension: &UsageRollupDimension,
    metrics: UsageRollupMetrics,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO usage_rollup_totals (
            dimension, dimension_key, dimension_label, requests, success_requests,
            error_requests, stream_requests, non_stream_requests, priced_requests,
            unpriced_requests, local_prompt_cache_requests, simulated_requests,
            upstream_metadata_requests, sticky_bound_requests, fallback_from_sticky_requests,
            total_input_tokens, billable_input_tokens, total_output_tokens,
            total_cache_read_input_tokens, total_cache_creation_input_tokens,
            local_prompt_cache_input_tokens, local_prompt_cache_read_input_tokens,
            local_prompt_cache_creation_input_tokens, total_estimated_cost_usd,
            total_original_cost_usd, total_kiro_metering_usage,
            external_pool_requests, external_pool_priced_requests, external_pool_unpriced_requests,
            external_pool_cost_floor_applied_requests, external_pool_raw_cost_usd,
            external_pool_shaped_cost_usd, external_pool_uplifted_cost_usd,
            external_pool_profit_usd, external_pool_reported_cost_usd,
            external_pool_billable_cost_usd, external_pool_cost_floor_delta_usd,
            duration_ms_sum, duration_ms_count, duration_ms_max, updated_at
        )
        VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
            $11, $12, $13, $14, $15, $16, $17, $18,
            $19, $20, $21, $22, $23, $24, $25, $26,
            $27, $28, $29, $30, $31, $32, $33, $34,
            $35, $36, $37, $38, $39, $40, now()
        )
        ON CONFLICT (dimension, dimension_key) DO UPDATE
        SET dimension_label = COALESCE(EXCLUDED.dimension_label, usage_rollup_totals.dimension_label),
            requests = usage_rollup_totals.requests + EXCLUDED.requests,
            success_requests = usage_rollup_totals.success_requests + EXCLUDED.success_requests,
            error_requests = usage_rollup_totals.error_requests + EXCLUDED.error_requests,
            stream_requests = usage_rollup_totals.stream_requests + EXCLUDED.stream_requests,
            non_stream_requests = usage_rollup_totals.non_stream_requests + EXCLUDED.non_stream_requests,
            priced_requests = usage_rollup_totals.priced_requests + EXCLUDED.priced_requests,
            unpriced_requests = usage_rollup_totals.unpriced_requests + EXCLUDED.unpriced_requests,
            local_prompt_cache_requests = usage_rollup_totals.local_prompt_cache_requests + EXCLUDED.local_prompt_cache_requests,
            simulated_requests = usage_rollup_totals.simulated_requests + EXCLUDED.simulated_requests,
            upstream_metadata_requests = usage_rollup_totals.upstream_metadata_requests + EXCLUDED.upstream_metadata_requests,
            sticky_bound_requests = usage_rollup_totals.sticky_bound_requests + EXCLUDED.sticky_bound_requests,
            fallback_from_sticky_requests = usage_rollup_totals.fallback_from_sticky_requests + EXCLUDED.fallback_from_sticky_requests,
            total_input_tokens = usage_rollup_totals.total_input_tokens + EXCLUDED.total_input_tokens,
            billable_input_tokens = usage_rollup_totals.billable_input_tokens + EXCLUDED.billable_input_tokens,
            total_output_tokens = usage_rollup_totals.total_output_tokens + EXCLUDED.total_output_tokens,
            total_cache_read_input_tokens = usage_rollup_totals.total_cache_read_input_tokens + EXCLUDED.total_cache_read_input_tokens,
            total_cache_creation_input_tokens = usage_rollup_totals.total_cache_creation_input_tokens + EXCLUDED.total_cache_creation_input_tokens,
            local_prompt_cache_input_tokens = usage_rollup_totals.local_prompt_cache_input_tokens + EXCLUDED.local_prompt_cache_input_tokens,
            local_prompt_cache_read_input_tokens = usage_rollup_totals.local_prompt_cache_read_input_tokens + EXCLUDED.local_prompt_cache_read_input_tokens,
            local_prompt_cache_creation_input_tokens = usage_rollup_totals.local_prompt_cache_creation_input_tokens + EXCLUDED.local_prompt_cache_creation_input_tokens,
            total_estimated_cost_usd = usage_rollup_totals.total_estimated_cost_usd + EXCLUDED.total_estimated_cost_usd,
            total_original_cost_usd = usage_rollup_totals.total_original_cost_usd + EXCLUDED.total_original_cost_usd,
            total_kiro_metering_usage = usage_rollup_totals.total_kiro_metering_usage + EXCLUDED.total_kiro_metering_usage,
            external_pool_requests = usage_rollup_totals.external_pool_requests + EXCLUDED.external_pool_requests,
            external_pool_priced_requests = usage_rollup_totals.external_pool_priced_requests + EXCLUDED.external_pool_priced_requests,
            external_pool_unpriced_requests = usage_rollup_totals.external_pool_unpriced_requests + EXCLUDED.external_pool_unpriced_requests,
            external_pool_cost_floor_applied_requests = usage_rollup_totals.external_pool_cost_floor_applied_requests + EXCLUDED.external_pool_cost_floor_applied_requests,
            external_pool_raw_cost_usd = usage_rollup_totals.external_pool_raw_cost_usd + EXCLUDED.external_pool_raw_cost_usd,
            external_pool_shaped_cost_usd = usage_rollup_totals.external_pool_shaped_cost_usd + EXCLUDED.external_pool_shaped_cost_usd,
            external_pool_uplifted_cost_usd = usage_rollup_totals.external_pool_uplifted_cost_usd + EXCLUDED.external_pool_uplifted_cost_usd,
            external_pool_profit_usd = usage_rollup_totals.external_pool_profit_usd + EXCLUDED.external_pool_profit_usd,
            external_pool_reported_cost_usd = usage_rollup_totals.external_pool_reported_cost_usd + EXCLUDED.external_pool_reported_cost_usd,
            external_pool_billable_cost_usd = usage_rollup_totals.external_pool_billable_cost_usd + EXCLUDED.external_pool_billable_cost_usd,
            external_pool_cost_floor_delta_usd = usage_rollup_totals.external_pool_cost_floor_delta_usd + EXCLUDED.external_pool_cost_floor_delta_usd,
            duration_ms_sum = usage_rollup_totals.duration_ms_sum + EXCLUDED.duration_ms_sum,
            duration_ms_count = usage_rollup_totals.duration_ms_count + EXCLUDED.duration_ms_count,
            duration_ms_max = CASE
                WHEN usage_rollup_totals.duration_ms_count + EXCLUDED.duration_ms_count <= 0 THEN 0
                ELSE GREATEST(usage_rollup_totals.duration_ms_max, EXCLUDED.duration_ms_max)
            END,
            updated_at = now()
        "#,
    )
    .bind(dimension.dimension)
    .bind(&dimension.key)
    .bind(&dimension.label)
    .bind(metrics.requests)
    .bind(metrics.success_requests)
    .bind(metrics.error_requests)
    .bind(metrics.stream_requests)
    .bind(metrics.non_stream_requests)
    .bind(metrics.priced_requests)
    .bind(metrics.unpriced_requests)
    .bind(metrics.local_prompt_cache_requests)
    .bind(metrics.simulated_requests)
    .bind(metrics.upstream_metadata_requests)
    .bind(metrics.sticky_bound_requests)
    .bind(metrics.fallback_from_sticky_requests)
    .bind(metrics.total_input_tokens)
    .bind(metrics.billable_input_tokens)
    .bind(metrics.total_output_tokens)
    .bind(metrics.total_cache_read_input_tokens)
    .bind(metrics.total_cache_creation_input_tokens)
    .bind(metrics.local_prompt_cache_input_tokens)
    .bind(metrics.local_prompt_cache_read_input_tokens)
    .bind(metrics.local_prompt_cache_creation_input_tokens)
    .bind(metrics.total_estimated_cost_usd)
    .bind(metrics.total_original_cost_usd)
    .bind(metrics.total_kiro_metering_usage)
    .bind(metrics.external_pool_requests)
    .bind(metrics.external_pool_priced_requests)
    .bind(metrics.external_pool_unpriced_requests)
    .bind(metrics.external_pool_cost_floor_applied_requests)
    .bind(metrics.external_pool_raw_cost_usd)
    .bind(metrics.external_pool_shaped_cost_usd)
    .bind(metrics.external_pool_uplifted_cost_usd)
    .bind(metrics.external_pool_profit_usd)
    .bind(metrics.external_pool_reported_cost_usd)
    .bind(metrics.external_pool_billable_cost_usd)
    .bind(metrics.external_pool_cost_floor_delta_usd)
    .bind(metrics.duration_ms_sum)
    .bind(metrics.duration_ms_count)
    .bind(metrics.duration_ms_max)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn upsert_usage_rollup_time_bucket(
    tx: &mut Transaction<'_, Postgres>,
    bucket_start: DateTime<Utc>,
    dimension: &UsageRollupDimension,
    metrics: UsageRollupMetrics,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO usage_rollup_time_buckets (
            bucket_start, dimension, dimension_key, dimension_label, requests,
            success_requests, error_requests, stream_requests, non_stream_requests,
            priced_requests, unpriced_requests, local_prompt_cache_requests,
            simulated_requests, upstream_metadata_requests, sticky_bound_requests,
            fallback_from_sticky_requests, total_input_tokens, billable_input_tokens,
            total_output_tokens, total_cache_read_input_tokens,
            total_cache_creation_input_tokens, local_prompt_cache_input_tokens,
            local_prompt_cache_read_input_tokens, local_prompt_cache_creation_input_tokens,
            total_estimated_cost_usd, total_original_cost_usd, total_kiro_metering_usage,
            external_pool_requests, external_pool_priced_requests,
            external_pool_unpriced_requests, external_pool_cost_floor_applied_requests,
            external_pool_raw_cost_usd, external_pool_shaped_cost_usd,
            external_pool_uplifted_cost_usd, external_pool_profit_usd, external_pool_reported_cost_usd,
            external_pool_billable_cost_usd, external_pool_cost_floor_delta_usd,
            duration_ms_sum, duration_ms_count,
            duration_ms_max, updated_at
        )
        VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
            $11, $12, $13, $14, $15, $16, $17, $18,
            $19, $20, $21, $22, $23, $24, $25, $26,
            $27, $28, $29, $30, $31, $32, $33, $34,
            $35, $36, $37, $38, $39, $40, $41, now()
        )
        ON CONFLICT (bucket_start, dimension, dimension_key) DO UPDATE
        SET dimension_label = COALESCE(EXCLUDED.dimension_label, usage_rollup_time_buckets.dimension_label),
            requests = usage_rollup_time_buckets.requests + EXCLUDED.requests,
            success_requests = usage_rollup_time_buckets.success_requests + EXCLUDED.success_requests,
            error_requests = usage_rollup_time_buckets.error_requests + EXCLUDED.error_requests,
            stream_requests = usage_rollup_time_buckets.stream_requests + EXCLUDED.stream_requests,
            non_stream_requests = usage_rollup_time_buckets.non_stream_requests + EXCLUDED.non_stream_requests,
            priced_requests = usage_rollup_time_buckets.priced_requests + EXCLUDED.priced_requests,
            unpriced_requests = usage_rollup_time_buckets.unpriced_requests + EXCLUDED.unpriced_requests,
            local_prompt_cache_requests = usage_rollup_time_buckets.local_prompt_cache_requests + EXCLUDED.local_prompt_cache_requests,
            simulated_requests = usage_rollup_time_buckets.simulated_requests + EXCLUDED.simulated_requests,
            upstream_metadata_requests = usage_rollup_time_buckets.upstream_metadata_requests + EXCLUDED.upstream_metadata_requests,
            sticky_bound_requests = usage_rollup_time_buckets.sticky_bound_requests + EXCLUDED.sticky_bound_requests,
            fallback_from_sticky_requests = usage_rollup_time_buckets.fallback_from_sticky_requests + EXCLUDED.fallback_from_sticky_requests,
            total_input_tokens = usage_rollup_time_buckets.total_input_tokens + EXCLUDED.total_input_tokens,
            billable_input_tokens = usage_rollup_time_buckets.billable_input_tokens + EXCLUDED.billable_input_tokens,
            total_output_tokens = usage_rollup_time_buckets.total_output_tokens + EXCLUDED.total_output_tokens,
            total_cache_read_input_tokens = usage_rollup_time_buckets.total_cache_read_input_tokens + EXCLUDED.total_cache_read_input_tokens,
            total_cache_creation_input_tokens = usage_rollup_time_buckets.total_cache_creation_input_tokens + EXCLUDED.total_cache_creation_input_tokens,
            local_prompt_cache_input_tokens = usage_rollup_time_buckets.local_prompt_cache_input_tokens + EXCLUDED.local_prompt_cache_input_tokens,
            local_prompt_cache_read_input_tokens = usage_rollup_time_buckets.local_prompt_cache_read_input_tokens + EXCLUDED.local_prompt_cache_read_input_tokens,
            local_prompt_cache_creation_input_tokens = usage_rollup_time_buckets.local_prompt_cache_creation_input_tokens + EXCLUDED.local_prompt_cache_creation_input_tokens,
            total_estimated_cost_usd = usage_rollup_time_buckets.total_estimated_cost_usd + EXCLUDED.total_estimated_cost_usd,
            total_original_cost_usd = usage_rollup_time_buckets.total_original_cost_usd + EXCLUDED.total_original_cost_usd,
            total_kiro_metering_usage = usage_rollup_time_buckets.total_kiro_metering_usage + EXCLUDED.total_kiro_metering_usage,
            external_pool_requests = usage_rollup_time_buckets.external_pool_requests + EXCLUDED.external_pool_requests,
            external_pool_priced_requests = usage_rollup_time_buckets.external_pool_priced_requests + EXCLUDED.external_pool_priced_requests,
            external_pool_unpriced_requests = usage_rollup_time_buckets.external_pool_unpriced_requests + EXCLUDED.external_pool_unpriced_requests,
            external_pool_cost_floor_applied_requests = usage_rollup_time_buckets.external_pool_cost_floor_applied_requests + EXCLUDED.external_pool_cost_floor_applied_requests,
            external_pool_raw_cost_usd = usage_rollup_time_buckets.external_pool_raw_cost_usd + EXCLUDED.external_pool_raw_cost_usd,
            external_pool_shaped_cost_usd = usage_rollup_time_buckets.external_pool_shaped_cost_usd + EXCLUDED.external_pool_shaped_cost_usd,
            external_pool_uplifted_cost_usd = usage_rollup_time_buckets.external_pool_uplifted_cost_usd + EXCLUDED.external_pool_uplifted_cost_usd,
            external_pool_profit_usd = usage_rollup_time_buckets.external_pool_profit_usd + EXCLUDED.external_pool_profit_usd,
            external_pool_reported_cost_usd = usage_rollup_time_buckets.external_pool_reported_cost_usd + EXCLUDED.external_pool_reported_cost_usd,
            external_pool_billable_cost_usd = usage_rollup_time_buckets.external_pool_billable_cost_usd + EXCLUDED.external_pool_billable_cost_usd,
            external_pool_cost_floor_delta_usd = usage_rollup_time_buckets.external_pool_cost_floor_delta_usd + EXCLUDED.external_pool_cost_floor_delta_usd,
            duration_ms_sum = usage_rollup_time_buckets.duration_ms_sum + EXCLUDED.duration_ms_sum,
            duration_ms_count = usage_rollup_time_buckets.duration_ms_count + EXCLUDED.duration_ms_count,
            duration_ms_max = CASE
                WHEN usage_rollup_time_buckets.duration_ms_count + EXCLUDED.duration_ms_count <= 0 THEN 0
                ELSE GREATEST(usage_rollup_time_buckets.duration_ms_max, EXCLUDED.duration_ms_max)
            END,
            updated_at = now()
        "#,
    )
    .bind(bucket_start)
    .bind(dimension.dimension)
    .bind(&dimension.key)
    .bind(&dimension.label)
    .bind(metrics.requests)
    .bind(metrics.success_requests)
    .bind(metrics.error_requests)
    .bind(metrics.stream_requests)
    .bind(metrics.non_stream_requests)
    .bind(metrics.priced_requests)
    .bind(metrics.unpriced_requests)
    .bind(metrics.local_prompt_cache_requests)
    .bind(metrics.simulated_requests)
    .bind(metrics.upstream_metadata_requests)
    .bind(metrics.sticky_bound_requests)
    .bind(metrics.fallback_from_sticky_requests)
    .bind(metrics.total_input_tokens)
    .bind(metrics.billable_input_tokens)
    .bind(metrics.total_output_tokens)
    .bind(metrics.total_cache_read_input_tokens)
    .bind(metrics.total_cache_creation_input_tokens)
    .bind(metrics.local_prompt_cache_input_tokens)
    .bind(metrics.local_prompt_cache_read_input_tokens)
    .bind(metrics.local_prompt_cache_creation_input_tokens)
    .bind(metrics.total_estimated_cost_usd)
    .bind(metrics.total_original_cost_usd)
    .bind(metrics.total_kiro_metering_usage)
    .bind(metrics.external_pool_requests)
    .bind(metrics.external_pool_priced_requests)
    .bind(metrics.external_pool_unpriced_requests)
    .bind(metrics.external_pool_cost_floor_applied_requests)
    .bind(metrics.external_pool_raw_cost_usd)
    .bind(metrics.external_pool_shaped_cost_usd)
    .bind(metrics.external_pool_uplifted_cost_usd)
    .bind(metrics.external_pool_profit_usd)
    .bind(metrics.external_pool_reported_cost_usd)
    .bind(metrics.external_pool_billable_cost_usd)
    .bind(metrics.external_pool_cost_floor_delta_usd)
    .bind(metrics.duration_ms_sum)
    .bind(metrics.duration_ms_count)
    .bind(metrics.duration_ms_max)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn upsert_usage_cache_read_total(
    tx: &mut Transaction<'_, Postgres>,
    cache_read_input_tokens: i32,
    direction: i64,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO usage_cache_read_totals (cache_read_input_tokens, requests, updated_at)
        VALUES ($1, $2, now())
        ON CONFLICT (cache_read_input_tokens) DO UPDATE
        SET requests = usage_cache_read_totals.requests + EXCLUDED.requests,
            updated_at = now()
        "#,
    )
    .bind(cache_read_input_tokens)
    .bind(direction)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn upsert_usage_cache_read_time_bucket(
    tx: &mut Transaction<'_, Postgres>,
    bucket_start: DateTime<Utc>,
    cache_read_input_tokens: i32,
    direction: i64,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO usage_cache_read_rollup_time_buckets (
            bucket_start, cache_read_input_tokens, requests, updated_at
        )
        VALUES ($1, $2, $3, now())
        ON CONFLICT (bucket_start, cache_read_input_tokens) DO UPDATE
        SET requests = usage_cache_read_rollup_time_buckets.requests + EXCLUDED.requests,
            updated_at = now()
        "#,
    )
    .bind(bucket_start)
    .bind(cache_read_input_tokens)
    .bind(direction)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn upsert_usage_duration_time_bucket(
    tx: &mut Transaction<'_, Postgres>,
    bucket_start: DateTime<Utc>,
    duration_ms: i32,
    direction: i64,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO usage_duration_rollup_time_buckets (
            bucket_start, duration_ms, requests, updated_at
        )
        VALUES ($1, $2, $3, now())
        ON CONFLICT (bucket_start, duration_ms) DO UPDATE
        SET requests = usage_duration_rollup_time_buckets.requests + EXCLUDED.requests,
            updated_at = now()
        "#,
    )
    .bind(bucket_start)
    .bind(duration_ms)
    .bind(direction)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn reconcile_usage_rollups_after_negative_delta(
    tx: &mut Transaction<'_, Postgres>,
    affected_total_dimensions: &[String],
    affected_total_keys: &[String],
    affected_time_buckets: &[DateTime<Utc>],
    affected_time_dimensions: &[String],
    affected_time_keys: &[String],
    affected_credentials: &[i64],
    affected_cache_reads: &[i32],
    affected_cache_buckets: &[DateTime<Utc>],
    affected_duration_buckets: &[DateTime<Utc>],
) -> anyhow::Result<()> {
    if !affected_total_dimensions.is_empty() {
        sqlx::query(
            r#"
            DELETE FROM usage_rollup_totals AS rollup
            USING unnest($1::text[], $2::text[]) AS affected(dimension, dimension_key)
            WHERE rollup.dimension = affected.dimension
              AND rollup.dimension_key = affected.dimension_key
              AND rollup.requests <= 0
            "#,
        )
        .bind(affected_total_dimensions)
        .bind(affected_total_keys)
        .execute(&mut **tx)
        .await?;
    }
    if !affected_time_buckets.is_empty() {
        sqlx::query(
            r#"
            DELETE FROM usage_rollup_time_buckets AS rollup
            USING unnest($1::timestamptz[], $2::text[], $3::text[])
                AS affected(bucket_start, dimension, dimension_key)
            WHERE rollup.bucket_start = affected.bucket_start
              AND rollup.dimension = affected.dimension
              AND rollup.dimension_key = affected.dimension_key
              AND rollup.requests <= 0
            "#,
        )
        .bind(affected_time_buckets)
        .bind(affected_time_dimensions)
        .bind(affected_time_keys)
        .execute(&mut **tx)
        .await?;
    }
    if !affected_credentials.is_empty() {
        sqlx::query(
            r#"
            DELETE FROM usage_credential_cost_summary
            WHERE credential_id = ANY($1)
              AND requests <= 0
            "#,
        )
        .bind(affected_credentials)
        .execute(&mut **tx)
        .await?;
    }
    if !affected_cache_reads.is_empty() {
        sqlx::query(
            r#"
            DELETE FROM usage_cache_read_totals
            WHERE cache_read_input_tokens = ANY($1)
              AND requests <= 0
            "#,
        )
        .bind(affected_cache_reads)
        .execute(&mut **tx)
        .await?;
    }
    if !affected_cache_buckets.is_empty() {
        sqlx::query(
            r#"
            DELETE FROM usage_cache_read_rollup_time_buckets
            WHERE bucket_start = ANY($1)
              AND requests <= 0
            "#,
        )
        .bind(affected_cache_buckets)
        .execute(&mut **tx)
        .await?;
    }
    if affected_duration_buckets.is_empty() {
        return Ok(());
    }

    sqlx::query(
        r#"
        DELETE FROM usage_duration_rollup_time_buckets
        WHERE bucket_start = ANY($1)
          AND requests <= 0
        "#,
    )
    .bind(affected_duration_buckets)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        r#"
        WITH affected AS (
            SELECT unnest($1::timestamptz[]) AS bucket_start
        ), maxima AS (
            SELECT
                affected.bucket_start,
                COALESCE(MAX(duration.duration_ms), 0)::bigint AS duration_ms_max
            FROM affected
            LEFT JOIN usage_duration_rollup_time_buckets AS duration
              ON duration.bucket_start = affected.bucket_start
             AND duration.requests > 0
            GROUP BY affected.bucket_start
        )
        UPDATE usage_rollup_time_buckets AS rollup
        SET duration_ms_max = maxima.duration_ms_max,
            updated_at = now()
        FROM maxima
        WHERE rollup.bucket_start = maxima.bucket_start
          AND rollup.dimension = 'global'
          AND rollup.dimension_key = 'all'
        "#,
    )
    .bind(affected_duration_buckets)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        r#"
        UPDATE usage_rollup_totals
        SET duration_ms_max = COALESCE((
                SELECT duration_ms::bigint
                FROM usage_duration_rollup_time_buckets
                WHERE requests > 0
                ORDER BY duration_ms DESC
                LIMIT 1
            ), 0),
            updated_at = now()
        WHERE dimension = 'global'
          AND dimension_key = 'all'
        "#,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn upsert_credential_usage_summary_delta(
    tx: &mut Transaction<'_, Postgres>,
    credential_id: u64,
    delta: CredentialUsageSummaryDelta,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO usage_credential_cost_summary (
            credential_id, requests, estimated_cost_usd, original_cost_usd, kiro_metering_usage,
            priced_requests, unpriced_requests, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, now())
        ON CONFLICT (credential_id) DO UPDATE
        SET requests = usage_credential_cost_summary.requests + EXCLUDED.requests,
            estimated_cost_usd = usage_credential_cost_summary.estimated_cost_usd + EXCLUDED.estimated_cost_usd,
            original_cost_usd = usage_credential_cost_summary.original_cost_usd + EXCLUDED.original_cost_usd,
            kiro_metering_usage = usage_credential_cost_summary.kiro_metering_usage + EXCLUDED.kiro_metering_usage,
            priced_requests = usage_credential_cost_summary.priced_requests + EXCLUDED.priced_requests,
            unpriced_requests = usage_credential_cost_summary.unpriced_requests + EXCLUDED.unpriced_requests,
            updated_at = now()
        "#,
    )
    .bind(credential_id as i64)
    .bind(delta.requests)
    .bind(delta.estimated_cost_usd)
    .bind(delta.original_cost_usd)
    .bind(delta.kiro_metering_usage)
    .bind(delta.priced_requests)
    .bind(delta.unpriced_requests)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn parse_usage_created_at(record: &UsageRecord) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&record.created_at)
        .map(|created_at| created_at.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

fn usage_rollup_bucket_start(created_at: DateTime<Utc>) -> DateTime<Utc> {
    created_at
        .with_minute(0)
        .expect("valid minute truncation")
        .with_second(0)
        .expect("valid second truncation")
        .with_nanosecond(0)
        .expect("valid nanosecond truncation")
}

fn non_empty_or_unknown(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        "unknown".to_string()
    } else {
        value.to_string()
    }
}

fn signed_bool(value: bool, sign: i64) -> i64 {
    if value { sign } else { 0 }
}

fn signed_i32(value: i32, sign: i64) -> i64 {
    value as i64 * sign
}

#[derive(Clone, Copy)]
enum DashboardBreakdownColumn {
    Status,
    UsageSource,
}

impl DashboardBreakdownColumn {
    fn rollup_dimension(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::UsageSource => "usage_source",
        }
    }

    fn label(self, value: &str) -> String {
        match self {
            Self::Status => usage_status_label(value),
            Self::UsageSource => usage_source_label(value),
        }
    }

    fn detail_column(self) -> &'static str {
        match self {
            Self::Status => "r.status",
            Self::UsageSource => "r.usage_source",
        }
    }
}

#[derive(Clone, Copy)]
enum DashboardTopGroup {
    Model,
    Credential,
    Endpoint,
    Error,
}

struct DashboardTopAggregateResult {
    items: Vec<UsageTopAggregate>,
    total: usize,
}

impl DashboardTopGroup {
    fn rollup_dimension(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Credential => "credential",
            Self::Endpoint => "endpoint",
            Self::Error => "error",
        }
    }

    fn rollup_extra_where(self) -> &'static str {
        match self {
            _ => "",
        }
    }

    fn detail_key_column(self) -> &'static str {
        match self {
            Self::Model => "COALESCE(NULLIF(BTRIM(r.model), ''), 'unknown')",
            Self::Credential => "r.credential_id::text",
            Self::Endpoint => "COALESCE(NULLIF(BTRIM(r.endpoint), ''), 'unknown')",
            Self::Error => "COALESCE(NULLIF(BTRIM(r.error_type), ''), r.status)",
        }
    }

    fn detail_label_column(self) -> &'static str {
        match self {
            Self::Credential => "NULLIF(BTRIM(r.credential_label), '')",
            Self::Error => "NULLIF(BTRIM(r.error_message), '')",
            _ => "NULL::text",
        }
    }

    fn detail_filter(self) -> Option<&'static str> {
        match self {
            Self::Credential => Some("r.credential_id IS NOT NULL"),
            Self::Error => Some("r.status <> 'success'"),
            _ => None,
        }
    }
}

fn normalize_query_limit(limit: usize) -> usize {
    if limit == 0 { 100 } else { limit.min(1000) }
}

fn normalize_page_limit(limit: usize) -> usize {
    if limit == 0 { 20 } else { limit.min(1000) }
}

fn push_usage_filters(builder: &mut QueryBuilder<'_, Postgres>, query: &UsageRecordQuery) {
    builder.push(" WHERE deleted_at IS NULL");

    if let Some(request_id) = query
        .request_id
        .as_deref()
        .map(str::trim)
        .filter(|request_id| !request_id.is_empty())
    {
        builder.push(" AND id = ");
        builder.push_bind(request_id.to_string());
    }

    if let Some(q) = query.q.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        let pattern = format!("%{}%", q);
        builder.push(" AND (");
        let fields = [
            "id",
            "created_at::text",
            "endpoint",
            "model",
            "conversation_id",
            "data->>'requestApiKeyId'",
            "credential_label",
            "status",
            "usage_source",
            "error_type",
            "error_message",
            "error_detail",
            "pricing_model",
            "credential_id::text",
            "data->>'upstreamModel'",
            "data->>'externalOutboundModel'",
            "data->>'externalPoolId'",
            "data->>'externalPoolName'",
            "data->>'routeKind'",
            "data->>'routeSubtype'",
            "data->>'modelResolutionSource'",
            "estimated_cost_usd::text",
            "kiro_metering_usage::text",
        ];
        for (index, field) in fields.iter().enumerate() {
            if index > 0 {
                builder.push(" OR ");
            }
            builder.push(*field).push(" ILIKE ");
            builder.push_bind(pattern.clone());
        }
        builder.push(")");
    }
    if let Some(endpoint) = query
        .endpoint
        .as_deref()
        .map(str::trim)
        .filter(|endpoint| !endpoint.is_empty())
    {
        builder.push(" AND endpoint ILIKE ");
        builder.push_bind(format!("%{}%", endpoint));
    }
    if let Some(conversation_id) = &query.conversation_id {
        builder.push(" AND conversation_id = ");
        builder.push_bind(conversation_id.clone());
    }
    if let Some(request_api_key_id) = query
        .request_api_key_id
        .as_deref()
        .map(str::trim)
        .filter(|request_api_key_id| !request_api_key_id.is_empty())
    {
        builder.push(" AND data->>'requestApiKeyId' = ");
        builder.push_bind(request_api_key_id.to_string());
    }
    if let Some(credential_id) = query.credential_id {
        builder.push(" AND credential_id = ");
        builder.push_bind(credential_id as i64);
    }
    if let Some(external_pool_id) = query.external_pool_id {
        builder.push(" AND data->>'externalPoolId' = ");
        builder.push_bind(external_pool_id.to_string());
    }
    if let Some(route_kind) = query.route_kind {
        builder.push(" AND data->>'routeKind' = ");
        builder.push_bind(usage_route_kind_value(route_kind));
    }
    if let Some(model) = &query.model {
        builder.push(" AND (model = ");
        builder.push_bind(model.clone());
        builder.push(" OR data->>'upstreamModel' = ");
        builder.push_bind(model.clone());
        builder.push(" OR data->>'externalOutboundModel' = ");
        builder.push_bind(model.clone());
        builder.push(")");
    }
    if let Some(status) = query.status {
        builder.push(" AND status = ");
        builder.push_bind(usage_status_value(status));
    }
    if let Some(source) = query.source {
        builder.push(" AND usage_source = ");
        builder.push_bind(usage_source_value(source));
    }
    if let Some(stream) = query.stream {
        builder.push(" AND stream = ");
        builder.push_bind(stream);
    }
    if let Some(min_cache_read) = query.min_cache_read {
        builder.push(" AND cache_read_input_tokens >= ");
        builder.push_bind(min_cache_read);
    }
    if let Some(min_first_token_latency_ms) = query.min_first_token_latency_ms {
        let min_first_token_latency_ms = min_first_token_latency_ms.min(i64::MAX as u64) as i64;
        builder.push(" AND duration_ms >= ");
        builder.push_bind(min_first_token_latency_ms);
        builder.push(" AND data ? 'firstTokenLatencyMs'");
        builder.push(" AND data->>'firstTokenLatencyMs' ~ '^[0-9]+$'");
        builder.push(" AND (data->>'firstTokenLatencyMs')::bigint >= ");
        builder.push_bind(min_first_token_latency_ms);
    }
    if let Some(since) = query.since {
        builder.push(" AND created_at >= ");
        builder.push_bind(since);
    }
    if let Some(until) = query.until {
        builder.push(" AND created_at <= ");
        builder.push_bind(until);
    }
}

fn push_dashboard_windows_cte(
    builder: &mut QueryBuilder<'_, Postgres>,
    specs: &[UsageDashboardWindowSpec],
) {
    builder.push("WITH windows(key, label, from_at, to_at, ord) AS (VALUES ");
    for (index, spec) in specs.iter().enumerate() {
        if index > 0 {
            builder.push(", ");
        }
        builder.push("(");
        builder.push_bind(spec.key.clone());
        builder.push("::text, ");
        builder.push_bind(spec.label.clone());
        builder.push("::text, ");
        builder.push_bind(spec.from);
        builder.push("::timestamptz, ");
        builder.push_bind(spec.to);
        builder.push("::timestamptz, ");
        builder.push_bind(usize_to_i64(index));
        builder.push("::bigint)");
    }
    builder.push(
        r#"), window_utc_hours AS (
        SELECT
            w.*,
            date_trunc('hour', w.from_at AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'
                AS from_hour_utc,
            date_trunc('hour', w.to_at AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'
                AS to_hour_utc
        FROM windows w
        ), window_bounds AS (
        SELECT
            h.*,
            CASE
                WHEN h.from_at = h.from_hour_utc THEN h.from_at
                ELSE h.from_hour_utc + interval '1 hour'
            END AS full_from_at,
            h.to_hour_utc AS full_to_at
        FROM window_utc_hours h
        ), window_boundary_ranges AS (
        SELECT
            w.key,
            w.ord,
            w.from_hour_utc AS boundary_hour,
            w.from_at AS boundary_from_at,
            LEAST(w.to_at, w.full_from_at) AS boundary_to_at
        FROM window_bounds w
        WHERE w.from_at < LEAST(w.to_at, w.full_from_at)

        UNION ALL

        SELECT
            w.key,
            w.ord,
            w.to_hour_utc AS boundary_hour,
            GREATEST(w.from_at, w.full_to_at) AS boundary_from_at,
            w.to_at AS boundary_to_at
        FROM window_bounds w
        WHERE GREATEST(w.from_at, w.full_to_at) < w.to_at
          AND GREATEST(w.from_at, w.full_to_at)
              >= LEAST(w.to_at, w.full_from_at)
        ), window_boundary_record_values AS (
        SELECT
            ranges.key AS window_key,
            ranges.ord AS window_ord,
            ranges.boundary_hour,
            records.status,
            records.usage_source,
            records.model,
            records.endpoint,
            records.credential_id,
            records.credential_label,
            records.error_type,
            records.error_message,
            records.stream,
            records.total_input_tokens,
            records.billable_input_tokens,
            records.output_tokens,
            records.cache_read_input_tokens,
            records.cache_creation_input_tokens,
            records.estimated_cost_usd,
            records.original_cost_usd,
            records.kiro_metering_usage,
            records.pricing_available,
            records.simulated,
            records.sticky_bound,
            records.fallback_from_sticky,
            records.data->>'externalPoolId' AS dashboard_external_pool_id,
            NULLIF(BTRIM(records.data->>'externalPoolName'), '')
                AS dashboard_external_pool_name,
            CASE
                WHEN jsonb_typeof(records.data->'durationMs') = 'number'
                 AND records.data->>'durationMs' ~ '^[0-9]+$'
                THEN LEAST(
                    (records.data->>'durationMs')::numeric,
                    9223372036854775807::numeric
                )::bigint
                ELSE GREATEST(records.duration_ms, 0)
            END AS dashboard_duration_ms,
            records.data->>'routeKind' = 'external_pool' AS dashboard_external_pool,
            jsonb_typeof(records.data->'externalPoolBilling') = 'object'
                AS dashboard_external_billing_present,
            COALESCE(
                records.data #> '{externalPoolBilling,pricingAvailable}' = 'true'::jsonb,
                false
            ) AS dashboard_external_pricing_available,
            COALESCE(
                records.data #> '{externalPoolBilling,costFloorApplied}' = 'true'::jsonb,
                false
            ) AS dashboard_external_cost_floor_applied,
            CASE
                WHEN jsonb_typeof(records.data #> '{externalPoolBilling,rawCostUsd}') = 'number'
                THEN (records.data #>> '{externalPoolBilling,rawCostUsd}')::double precision
                ELSE 0::double precision
            END AS dashboard_external_raw_cost_usd,
            CASE
                WHEN jsonb_typeof(records.data #> '{externalPoolBilling,shapedCostUsd}') = 'number'
                THEN (records.data #>> '{externalPoolBilling,shapedCostUsd}')::double precision
                ELSE 0::double precision
            END AS dashboard_external_shaped_cost_usd,
            CASE
                WHEN jsonb_typeof(records.data #> '{externalPoolBilling,upliftedCostUsd}') = 'number'
                THEN (records.data #>> '{externalPoolBilling,upliftedCostUsd}')::double precision
                ELSE 0::double precision
            END AS dashboard_external_uplifted_cost_usd,
            CASE
                WHEN jsonb_typeof(records.data #> '{externalPoolBilling,reportedCostUsd}') = 'number'
                THEN (records.data #>> '{externalPoolBilling,reportedCostUsd}')::double precision
                ELSE 0::double precision
            END AS dashboard_external_reported_cost_usd,
            CASE
                WHEN jsonb_typeof(records.data #> '{externalPoolBilling,billableCostUsd}') = 'number'
                THEN (records.data #>> '{externalPoolBilling,billableCostUsd}')::double precision
                ELSE 0::double precision
            END AS dashboard_external_billable_cost_usd,
            CASE
                WHEN jsonb_typeof(records.data #> '{externalPoolBilling,costFloorDeltaUsd}') = 'number'
                THEN (records.data #>> '{externalPoolBilling,costFloorDeltaUsd}')::double precision
                ELSE 0::double precision
            END AS dashboard_external_cost_floor_delta_usd
        FROM window_boundary_ranges ranges
        JOIN usage_records records
          ON records.created_at >= ranges.boundary_from_at
         AND records.created_at < ranges.boundary_to_at
         AND records.deleted_at IS NULL
         AND records.rollup_active
        ), window_boundary_records AS (
        SELECT
            detail_values.*,
            CASE
                WHEN detail_values.dashboard_external_pricing_available
                 AND detail_values.dashboard_external_shaped_cost_usd = 0
                 AND detail_values.dashboard_external_reported_cost_usd > 0
                THEN detail_values.dashboard_external_reported_cost_usd
                ELSE detail_values.dashboard_external_shaped_cost_usd
            END AS dashboard_external_effective_shaped_cost_usd,
            CASE
                WHEN detail_values.dashboard_external_pricing_available
                 AND detail_values.dashboard_external_uplifted_cost_usd = 0
                 AND detail_values.dashboard_external_reported_cost_usd > 0
                THEN detail_values.dashboard_external_reported_cost_usd
                ELSE detail_values.dashboard_external_uplifted_cost_usd
            END AS dashboard_external_effective_uplifted_cost_usd
        FROM window_boundary_record_values detail_values
        ) "#,
    );
}

fn push_dashboard_global_segments_cte(builder: &mut QueryBuilder<'_, Postgres>) {
    builder.push(
        r#", dashboard_global_segment_rows AS (
        SELECT
            w.key AS window_key,
            date_trunc('hour', buckets.bucket_start AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'
                AS segment_hour,
            buckets.requests,
            buckets.success_requests,
            buckets.error_requests,
            buckets.stream_requests,
            buckets.non_stream_requests,
            buckets.total_input_tokens,
            buckets.billable_input_tokens,
            buckets.total_output_tokens,
            buckets.total_cache_read_input_tokens,
            buckets.total_cache_creation_input_tokens,
            buckets.total_estimated_cost_usd,
            buckets.total_original_cost_usd,
            buckets.total_kiro_metering_usage,
            buckets.priced_requests,
            buckets.unpriced_requests,
            buckets.sticky_bound_requests,
            buckets.fallback_from_sticky_requests,
            buckets.simulated_requests,
            buckets.upstream_metadata_requests,
            buckets.external_pool_requests,
            buckets.external_pool_priced_requests,
            buckets.external_pool_unpriced_requests,
            buckets.external_pool_cost_floor_applied_requests,
            buckets.external_pool_raw_cost_usd,
            buckets.external_pool_shaped_cost_usd,
            buckets.external_pool_uplifted_cost_usd,
            buckets.external_pool_profit_usd,
            buckets.external_pool_reported_cost_usd,
            buckets.external_pool_billable_cost_usd,
            buckets.external_pool_cost_floor_delta_usd,
            buckets.duration_ms_sum::numeric AS duration_ms_sum,
            buckets.duration_ms_count
        FROM window_bounds w
        JOIN usage_rollup_time_buckets buckets
          ON buckets.dimension = 'global'
         AND buckets.dimension_key = 'all'
         AND buckets.bucket_start >= w.full_from_at
         AND buckets.bucket_start < w.full_to_at

        UNION ALL

        SELECT
            records.window_key,
            records.boundary_hour AS segment_hour,
            COUNT(*)::bigint AS requests,
            COUNT(*) FILTER (WHERE records.status = 'success')::bigint AS success_requests,
            COUNT(*) FILTER (WHERE records.status <> 'success')::bigint AS error_requests,
            COUNT(*) FILTER (WHERE records.stream)::bigint AS stream_requests,
            COUNT(*) FILTER (WHERE NOT records.stream)::bigint AS non_stream_requests,
            COALESCE(SUM(records.total_input_tokens), 0)::bigint AS total_input_tokens,
            COALESCE(SUM(records.billable_input_tokens), 0)::bigint AS billable_input_tokens,
            COALESCE(SUM(records.output_tokens), 0)::bigint AS total_output_tokens,
            COALESCE(SUM(records.cache_read_input_tokens), 0)::bigint AS total_cache_read_input_tokens,
            COALESCE(SUM(records.cache_creation_input_tokens), 0)::bigint AS total_cache_creation_input_tokens,
            COALESCE(SUM(records.estimated_cost_usd), 0)::double precision AS total_estimated_cost_usd,
            COALESCE(SUM(records.original_cost_usd), 0)::double precision AS total_original_cost_usd,
            COALESCE(SUM(records.kiro_metering_usage), 0)::double precision
                AS total_kiro_metering_usage,
            COUNT(*) FILTER (WHERE records.pricing_available)::bigint AS priced_requests,
            COUNT(*) FILTER (WHERE NOT records.pricing_available)::bigint AS unpriced_requests,
            COUNT(*) FILTER (WHERE records.sticky_bound)::bigint AS sticky_bound_requests,
            COUNT(*) FILTER (WHERE records.fallback_from_sticky)::bigint AS fallback_from_sticky_requests,
            COUNT(*) FILTER (WHERE records.simulated)::bigint AS simulated_requests,
            COUNT(*) FILTER (WHERE records.usage_source = 'upstream_metadata')::bigint
                AS upstream_metadata_requests,
            COUNT(*) FILTER (WHERE records.dashboard_external_pool)::bigint
                AS external_pool_requests,
            COUNT(*) FILTER (
                WHERE records.dashboard_external_pool
                  AND records.dashboard_external_billing_present
                  AND records.dashboard_external_pricing_available
            )::bigint AS external_pool_priced_requests,
            COUNT(*) FILTER (
                WHERE records.dashboard_external_pool
                  AND NOT (
                      records.dashboard_external_billing_present
                      AND records.dashboard_external_pricing_available
                  )
            )::bigint AS external_pool_unpriced_requests,
            COUNT(*) FILTER (
                WHERE records.dashboard_external_billing_present
                  AND records.dashboard_external_cost_floor_applied
            )::bigint AS external_pool_cost_floor_applied_requests,
            COALESCE(SUM(records.dashboard_external_raw_cost_usd), 0)::double precision
                AS external_pool_raw_cost_usd,
            COALESCE(SUM(records.dashboard_external_effective_shaped_cost_usd), 0)::double precision
                AS external_pool_shaped_cost_usd,
            COALESCE(SUM(records.dashboard_external_effective_uplifted_cost_usd), 0)::double precision
                AS external_pool_uplifted_cost_usd,
            COALESCE(SUM(
                records.dashboard_external_effective_uplifted_cost_usd
                - records.dashboard_external_raw_cost_usd
            ), 0)::double precision AS external_pool_profit_usd,
            COALESCE(SUM(records.dashboard_external_reported_cost_usd), 0)::double precision
                AS external_pool_reported_cost_usd,
            COALESCE(SUM(records.dashboard_external_billable_cost_usd), 0)::double precision
                AS external_pool_billable_cost_usd,
            COALESCE(SUM(records.dashboard_external_cost_floor_delta_usd), 0)::double precision
                AS external_pool_cost_floor_delta_usd,
            COALESCE(SUM(records.dashboard_duration_ms::numeric), 0)::numeric AS duration_ms_sum,
            COUNT(*)::bigint AS duration_ms_count
        FROM window_boundary_records records
        GROUP BY records.window_key, records.boundary_hour
        ), dashboard_global_segments AS (
        SELECT
            segment_rows.window_key,
            segment_rows.segment_hour,
            SUM(segment_rows.requests)::bigint AS requests,
            SUM(segment_rows.success_requests)::bigint AS success_requests,
            SUM(segment_rows.error_requests)::bigint AS error_requests,
            SUM(segment_rows.stream_requests)::bigint AS stream_requests,
            SUM(segment_rows.non_stream_requests)::bigint AS non_stream_requests,
            SUM(segment_rows.total_input_tokens)::bigint AS total_input_tokens,
            SUM(segment_rows.billable_input_tokens)::bigint AS billable_input_tokens,
            SUM(segment_rows.total_output_tokens)::bigint AS total_output_tokens,
            SUM(segment_rows.total_cache_read_input_tokens)::bigint AS total_cache_read_input_tokens,
            SUM(segment_rows.total_cache_creation_input_tokens)::bigint
                AS total_cache_creation_input_tokens,
            SUM(segment_rows.total_estimated_cost_usd)::double precision
                AS total_estimated_cost_usd,
            SUM(segment_rows.total_original_cost_usd)::double precision
                AS total_original_cost_usd,
            SUM(segment_rows.total_kiro_metering_usage)::double precision
                AS total_kiro_metering_usage,
            SUM(segment_rows.priced_requests)::bigint AS priced_requests,
            SUM(segment_rows.unpriced_requests)::bigint AS unpriced_requests,
            SUM(segment_rows.sticky_bound_requests)::bigint AS sticky_bound_requests,
            SUM(segment_rows.fallback_from_sticky_requests)::bigint
                AS fallback_from_sticky_requests,
            SUM(segment_rows.simulated_requests)::bigint AS simulated_requests,
            SUM(segment_rows.upstream_metadata_requests)::bigint AS upstream_metadata_requests,
            SUM(segment_rows.external_pool_requests)::bigint AS external_pool_requests,
            SUM(segment_rows.external_pool_priced_requests)::bigint
                AS external_pool_priced_requests,
            SUM(segment_rows.external_pool_unpriced_requests)::bigint
                AS external_pool_unpriced_requests,
            SUM(segment_rows.external_pool_cost_floor_applied_requests)::bigint
                AS external_pool_cost_floor_applied_requests,
            SUM(segment_rows.external_pool_raw_cost_usd)::double precision
                AS external_pool_raw_cost_usd,
            SUM(segment_rows.external_pool_shaped_cost_usd)::double precision
                AS external_pool_shaped_cost_usd,
            SUM(segment_rows.external_pool_uplifted_cost_usd)::double precision
                AS external_pool_uplifted_cost_usd,
            SUM(segment_rows.external_pool_profit_usd)::double precision
                AS external_pool_profit_usd,
            SUM(segment_rows.external_pool_reported_cost_usd)::double precision
                AS external_pool_reported_cost_usd,
            SUM(segment_rows.external_pool_billable_cost_usd)::double precision
                AS external_pool_billable_cost_usd,
            SUM(segment_rows.external_pool_cost_floor_delta_usd)::double precision
                AS external_pool_cost_floor_delta_usd,
            SUM(segment_rows.duration_ms_sum)::numeric AS duration_ms_sum,
            SUM(segment_rows.duration_ms_count)::bigint AS duration_ms_count
        FROM dashboard_global_segment_rows segment_rows
        GROUP BY segment_rows.window_key, segment_rows.segment_hour
        ) "#,
    );
}

fn push_dashboard_external_pool_segments_cte(builder: &mut QueryBuilder<'_, Postgres>) {
    builder.push(
        r#", dashboard_external_pool_segment_rows AS (
        SELECT
            w.key AS window_key,
            date_trunc('hour', buckets.bucket_start AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'
                AS segment_hour,
            buckets.dimension_key AS pool_id,
            buckets.dimension_label AS pool_name,
            buckets.external_pool_requests,
            buckets.external_pool_priced_requests,
            buckets.external_pool_unpriced_requests,
            buckets.external_pool_cost_floor_applied_requests,
            buckets.external_pool_raw_cost_usd,
            buckets.external_pool_shaped_cost_usd,
            buckets.external_pool_uplifted_cost_usd,
            buckets.external_pool_profit_usd,
            buckets.external_pool_reported_cost_usd,
            buckets.external_pool_billable_cost_usd,
            buckets.external_pool_cost_floor_delta_usd
        FROM window_bounds w
        JOIN usage_rollup_time_buckets buckets
          ON buckets.dimension = 'external_pool'
         AND buckets.bucket_start >= w.full_from_at
         AND buckets.bucket_start < w.full_to_at

        UNION ALL

        SELECT
            records.window_key,
            records.boundary_hour AS segment_hour,
            records.dashboard_external_pool_id AS pool_id,
            MAX(records.dashboard_external_pool_name) AS pool_name,
            COUNT(*) FILTER (WHERE records.dashboard_external_pool)::bigint
                AS external_pool_requests,
            COUNT(*) FILTER (
                WHERE records.dashboard_external_pool
                  AND records.dashboard_external_billing_present
                  AND records.dashboard_external_pricing_available
            )::bigint AS external_pool_priced_requests,
            COUNT(*) FILTER (
                WHERE records.dashboard_external_pool
                  AND NOT (
                      records.dashboard_external_billing_present
                      AND records.dashboard_external_pricing_available
                  )
            )::bigint AS external_pool_unpriced_requests,
            COUNT(*) FILTER (
                WHERE records.dashboard_external_billing_present
                  AND records.dashboard_external_cost_floor_applied
            )::bigint AS external_pool_cost_floor_applied_requests,
            COALESCE(SUM(records.dashboard_external_raw_cost_usd), 0)::double precision
                AS external_pool_raw_cost_usd,
            COALESCE(SUM(records.dashboard_external_effective_shaped_cost_usd), 0)::double precision
                AS external_pool_shaped_cost_usd,
            COALESCE(SUM(records.dashboard_external_effective_uplifted_cost_usd), 0)::double precision
                AS external_pool_uplifted_cost_usd,
            COALESCE(SUM(
                records.dashboard_external_effective_uplifted_cost_usd
                - records.dashboard_external_raw_cost_usd
            ), 0)::double precision AS external_pool_profit_usd,
            COALESCE(SUM(records.dashboard_external_reported_cost_usd), 0)::double precision
                AS external_pool_reported_cost_usd,
            COALESCE(SUM(records.dashboard_external_billable_cost_usd), 0)::double precision
                AS external_pool_billable_cost_usd,
            COALESCE(SUM(records.dashboard_external_cost_floor_delta_usd), 0)::double precision
                AS external_pool_cost_floor_delta_usd
        FROM window_boundary_records records
        WHERE records.dashboard_external_pool_id ~ '^[0-9]+$'
        GROUP BY
            records.window_key,
            records.boundary_hour,
            records.dashboard_external_pool_id
        ), dashboard_external_pool_segments AS (
        SELECT
            segment_rows.window_key,
            segment_rows.segment_hour,
            segment_rows.pool_id,
            MAX(segment_rows.pool_name) AS pool_name,
            SUM(segment_rows.external_pool_requests)::bigint AS external_pool_requests,
            SUM(segment_rows.external_pool_priced_requests)::bigint
                AS external_pool_priced_requests,
            SUM(segment_rows.external_pool_unpriced_requests)::bigint
                AS external_pool_unpriced_requests,
            SUM(segment_rows.external_pool_cost_floor_applied_requests)::bigint
                AS external_pool_cost_floor_applied_requests,
            SUM(segment_rows.external_pool_raw_cost_usd)::double precision
                AS external_pool_raw_cost_usd,
            SUM(segment_rows.external_pool_shaped_cost_usd)::double precision
                AS external_pool_shaped_cost_usd,
            SUM(segment_rows.external_pool_uplifted_cost_usd)::double precision
                AS external_pool_uplifted_cost_usd,
            SUM(segment_rows.external_pool_profit_usd)::double precision
                AS external_pool_profit_usd,
            SUM(segment_rows.external_pool_reported_cost_usd)::double precision
                AS external_pool_reported_cost_usd,
            SUM(segment_rows.external_pool_billable_cost_usd)::double precision
                AS external_pool_billable_cost_usd,
            SUM(segment_rows.external_pool_cost_floor_delta_usd)::double precision
                AS external_pool_cost_floor_delta_usd
        FROM dashboard_external_pool_segment_rows segment_rows
        GROUP BY segment_rows.window_key, segment_rows.segment_hour, segment_rows.pool_id
        ) "#,
    );
}

fn row_i64_to_usize(row: &PgRow, column: &str) -> anyhow::Result<usize> {
    let value: i64 = row.try_get(column)?;
    Ok(value.max(0) as usize)
}

fn row_i64_to_u64(row: &PgRow, column: &str) -> anyhow::Result<u64> {
    let value: i64 = row.try_get(column)?;
    Ok(value.max(0) as u64)
}

fn dashboard_window_from_row(row: PgRow) -> anyhow::Result<UsageDashboardWindow> {
    let from: DateTime<Utc> = row.try_get("from_at")?;
    let to: DateTime<Utc> = row.try_get("to_at")?;
    let total_requests = row_i64_to_usize(&row, "total_requests")?;
    let error_requests = row_i64_to_usize(&row, "error_requests")?;
    let total_input_tokens: i64 = row.try_get("total_input_tokens")?;
    let total_cache_read_input_tokens: i64 = row.try_get("total_cache_read_input_tokens")?;
    let p95_duration_ms: i64 = row.try_get("p95_duration_ms")?;

    Ok(UsageDashboardWindow {
        key: row.try_get("key")?,
        label: row.try_get("label")?,
        from: from.to_rfc3339(),
        to: to.to_rfc3339(),
        summary: UsageDashboardSummary {
            total_requests,
            success_requests: row_i64_to_usize(&row, "success_requests")?,
            error_requests,
            error_rate: usage_ratio(error_requests, total_requests),
            stream_requests: row_i64_to_usize(&row, "stream_requests")?,
            non_stream_requests: row_i64_to_usize(&row, "non_stream_requests")?,
            high_cache_requests: row_i64_to_usize(&row, "high_cache_requests")?,
            total_input_tokens,
            billable_input_tokens: row.try_get("billable_input_tokens")?,
            total_output_tokens: row.try_get("total_output_tokens")?,
            total_cache_read_input_tokens,
            total_cache_creation_input_tokens: row.try_get("total_cache_creation_input_tokens")?,
            cache_read_ratio: token_ratio(total_cache_read_input_tokens, total_input_tokens),
            total_estimated_cost_usd: row.try_get("total_estimated_cost_usd")?,
            total_original_cost_usd: row.try_get("total_original_cost_usd")?,
            total_kiro_metering_usage: row.try_get("total_kiro_metering_usage")?,
            priced_requests: row_i64_to_usize(&row, "priced_requests")?,
            unpriced_requests: row_i64_to_usize(&row, "unpriced_requests")?,
            average_duration_ms: row.try_get("average_duration_ms")?,
            p95_duration_ms: p95_duration_ms.max(0) as u64,
            sticky_bound_requests: row_i64_to_usize(&row, "sticky_bound_requests")?,
            fallback_from_sticky_requests: row_i64_to_usize(&row, "fallback_from_sticky_requests")?,
            simulated_requests: row_i64_to_usize(&row, "simulated_requests")?,
            upstream_metadata_requests: row_i64_to_usize(&row, "upstream_metadata_requests")?,
            external_pool_billing: UsageExternalPoolBillingSummary {
                requests: row_i64_to_usize(&row, "external_pool_requests")?,
                priced_requests: row_i64_to_usize(&row, "external_pool_priced_requests")?,
                unpriced_requests: row_i64_to_usize(&row, "external_pool_unpriced_requests")?,
                cost_floor_applied_requests: row_i64_to_usize(
                    &row,
                    "external_pool_cost_floor_applied_requests",
                )?,
                raw_cost_usd: row.try_get("external_pool_raw_cost_usd")?,
                shaped_cost_usd: row.try_get("external_pool_shaped_cost_usd")?,
                uplifted_cost_usd: row.try_get("external_pool_uplifted_cost_usd")?,
                profit_usd: row.try_get("external_pool_profit_usd")?,
                reported_cost_usd: row.try_get("external_pool_reported_cost_usd")?,
                billable_cost_usd: row.try_get("external_pool_billable_cost_usd")?,
                cost_floor_delta_usd: row.try_get("external_pool_cost_floor_delta_usd")?,
            },
            external_pool_billing_by_pool: Vec::new(),
            status_breakdown: Vec::new(),
            usage_source_breakdown: Vec::new(),
        },
    })
}

fn usage_dashboard_window_from_series_point(point: UsageSeriesPoint) -> UsageDashboardWindow {
    let total_requests = point.requests;
    let error_requests = point.error_requests;
    UsageDashboardWindow {
        key: point.key,
        label: point.label,
        from: point.from,
        to: point.to,
        summary: UsageDashboardSummary {
            total_requests,
            success_requests: point.success_requests,
            error_requests,
            error_rate: usage_ratio(error_requests, total_requests),
            stream_requests: 0,
            non_stream_requests: 0,
            high_cache_requests: 0,
            total_input_tokens: point.total_input_tokens,
            billable_input_tokens: point.billable_input_tokens,
            total_output_tokens: point.total_output_tokens,
            total_cache_read_input_tokens: 0,
            total_cache_creation_input_tokens: 0,
            cache_read_ratio: 0.0,
            total_estimated_cost_usd: point.total_estimated_cost_usd,
            total_original_cost_usd: point.total_original_cost_usd,
            total_kiro_metering_usage: point.total_kiro_metering_usage,
            priced_requests: 0,
            unpriced_requests: 0,
            average_duration_ms: 0.0,
            p95_duration_ms: 0,
            sticky_bound_requests: 0,
            fallback_from_sticky_requests: 0,
            simulated_requests: 0,
            upstream_metadata_requests: 0,
            external_pool_billing: UsageExternalPoolBillingSummary::default(),
            external_pool_billing_by_pool: Vec::new(),
            status_breakdown: Vec::new(),
            usage_source_breakdown: Vec::new(),
        },
    }
}

fn series_point_from_row(row: PgRow) -> anyhow::Result<UsageSeriesPoint> {
    let from: DateTime<Utc> = row.try_get("from_at")?;
    let to: DateTime<Utc> = row.try_get("to_at")?;
    Ok(UsageSeriesPoint {
        key: row.try_get("key")?,
        label: row.try_get("label")?,
        from: from.to_rfc3339(),
        to: to.to_rfc3339(),
        requests: row_i64_to_usize(&row, "requests")?,
        success_requests: row_i64_to_usize(&row, "success_requests")?,
        error_requests: row_i64_to_usize(&row, "error_requests")?,
        total_input_tokens: row.try_get("total_input_tokens")?,
        billable_input_tokens: row.try_get("billable_input_tokens")?,
        total_output_tokens: row.try_get("total_output_tokens")?,
        total_estimated_cost_usd: row.try_get("total_estimated_cost_usd")?,
        total_original_cost_usd: row.try_get("total_original_cost_usd")?,
        total_kiro_metering_usage: row.try_get("total_kiro_metering_usage")?,
    })
}

fn usage_aggregate_from_row(row: PgRow) -> anyhow::Result<UsageAggregate> {
    Ok(UsageAggregate {
        key: row.try_get("key")?,
        label: row.try_get("label")?,
        requests: row_i64_to_usize(&row, "requests")?,
        cache_read_input_tokens: row.try_get("cache_read_input_tokens")?,
        cache_creation_input_tokens: row.try_get("cache_creation_input_tokens")?,
        estimated_cost_usd: row.try_get("estimated_cost_usd")?,
        original_cost_usd: row.try_get("original_cost_usd")?,
    })
}

fn usage_top_aggregate_from_row(row: PgRow) -> anyhow::Result<UsageTopAggregate> {
    Ok(UsageTopAggregate {
        key: row.try_get("key")?,
        label: row.try_get("label")?,
        requests: row_i64_to_usize(&row, "requests")?,
        error_requests: row_i64_to_usize(&row, "error_requests")?,
        total_input_tokens: row.try_get("total_input_tokens")?,
        billable_input_tokens: row.try_get("billable_input_tokens")?,
        total_output_tokens: row.try_get("total_output_tokens")?,
        total_cache_read_input_tokens: row.try_get("total_cache_read_input_tokens")?,
        total_cache_creation_input_tokens: row.try_get("total_cache_creation_input_tokens")?,
        total_estimated_cost_usd: row.try_get("total_estimated_cost_usd")?,
        total_original_cost_usd: row.try_get("total_original_cost_usd")?,
        total_kiro_metering_usage: row.try_get("total_kiro_metering_usage")?,
    })
}

fn usage_ratio(part: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        part as f64 / total as f64
    }
}

fn token_ratio(part: i64, total: i64) -> f64 {
    if total <= 0 {
        0.0
    } else {
        part.max(0) as f64 / total as f64
    }
}

fn usage_status_label(value: &str) -> String {
    match value {
        "success" => "成功",
        "error" => "错误",
        "stream_error" => "流错误",
        "upstream_timeout" => "上游超时",
        "client_dropped" => "客户端断开",
        _ => value,
    }
    .to_string()
}

fn usage_source_label(value: &str) -> String {
    match value {
        "upstream_metadata" => "上游 metadata",
        "local_prompt_cache" => "本地 prompt cache",
        "context_estimate" => "上下文估算",
        "request_estimate" => "请求估算",
        "none" => "无缓存",
        _ => value,
    }
    .to_string()
}

#[allow(dead_code)]
fn runtime_state_from_row(row: &PgRow) -> anyhow::Result<CredentialRuntimeStateRow> {
    let failure_count: i32 = row.try_get("failure_count")?;
    let refresh_failure_count: i32 = row.try_get("refresh_failure_count")?;
    let disabled_reason: Option<String> = row.try_get("disabled_reason")?;
    let warmup_remaining: i32 = row.try_get("warmup_remaining")?;
    let generation: i64 = row.try_get("generation")?;
    let revision: i64 = row.try_get("revision")?;
    Ok(CredentialRuntimeStateRow {
        failure_count: failure_count.max(0) as u32,
        refresh_failure_count: refresh_failure_count.max(0) as u32,
        disabled_reason,
        warmup_remaining: warmup_remaining.max(0) as u32,
        generation: generation.max(0) as u64,
        revision: revision.max(0) as u64,
    })
}

fn external_pool_eligibility_from_row(row: &PgRow) -> anyhow::Result<ExternalPoolEligibility> {
    let revision: i64 = row.try_get("revision")?;
    if revision <= 0 {
        anyhow::bail!("revision 必须为正数");
    }
    let base_url: String = row.try_get("base_url")?;
    let parsed_base_url =
        url::Url::parse(base_url.trim()).map_err(|_| anyhow::anyhow!("base_url 不是有效 URL"))?;
    if !matches!(parsed_base_url.scheme(), "http" | "https") {
        anyhow::bail!("base_url scheme 必须是 http 或 https");
    }
    if parsed_base_url.host_str().is_none() {
        anyhow::bail!("base_url 必须包含 host");
    }
    if !row.try_get::<bool, _>("api_key_present")? {
        anyhow::bail!("api_key 不能为空");
    }
    let auth_type: String = row.try_get("auth_type")?;
    ExternalPoolAuthType::parse_known(&auth_type)
        .ok_or_else(|| anyhow::anyhow!("auth_type 值无效"))?;
    let max_concurrent_requests: i32 = row.try_get("max_concurrent_requests")?;
    if max_concurrent_requests <= 0 {
        anyhow::bail!("max_concurrent_requests 必须为正数");
    }
    let usage_projection_mode: String = row.try_get("usage_projection_mode")?;
    ExternalPoolUsageProjectionMode::parse_known(&usage_projection_mode)
        .ok_or_else(|| anyhow::anyhow!("usage_projection_mode 值无效"))?;
    let stream_response_mode: Option<String> = row.try_get("stream_response_mode")?;
    if stream_response_mode
        .as_deref()
        .is_some_and(|value| ExternalPoolStreamResponseMode::parse_known(value).is_none())
    {
        anyhow::bail!("stream_response_mode 值无效");
    }
    let request_body_mode: String = row
        .try_get("request_body_mode")
        .map_err(|_| anyhow::anyhow!("request_body_mode 字段类型无效"))?;
    let _request_body_mode = ExternalPoolRequestBodyMode::parse_known(&request_body_mode)
        .ok_or_else(|| anyhow::anyhow!("request_body_mode 值无效"))?;
    let raw_model_mode: String = row.try_get("raw_model_mode")?;
    ExternalPoolRawModelMode::parse_known(&raw_model_mode)
        .ok_or_else(|| anyhow::anyhow!("raw_model_mode 值无效"))?;
    let auto_disable_policy: String = row.try_get("auto_disable_policy")?;
    ExternalPoolAutoDisablePolicy::parse_known(&auto_disable_policy)
        .ok_or_else(|| anyhow::anyhow!("auto_disable_policy 值无效"))?;
    let pre_output_stream_retry_mode: String = row.try_get("pre_output_stream_retry_mode")?;
    ExternalPoolStreamRetryMode::parse_known(&pre_output_stream_retry_mode)
        .ok_or_else(|| anyhow::anyhow!("pre_output_stream_retry_mode 值无效"))?;
    let header_profile: String = row.try_get("header_profile")?;
    ExternalPoolHeaderProfile::parse_known(&header_profile)
        .ok_or_else(|| anyhow::anyhow!("header_profile 值无效"))?;
    row.try_get::<bool, _>("append_beta_query")?;
    let header_overrides_value: serde_json::Value = row
        .try_get("header_overrides")
        .map_err(|_| anyhow::anyhow!("header_overrides 字段类型无效"))?;
    let header_overrides =
        serde_json::from_value::<HashMap<String, String>>(header_overrides_value)
            .map_err(|_| anyhow::anyhow!("header_overrides 必须是对象"))?;
    normalize_external_pool_header_overrides(header_overrides)?;
    let wire_profile: String = row.try_get("wire_profile")?;
    ExternalPoolWireProfile::parse_known(&wire_profile)
        .ok_or_else(|| anyhow::anyhow!("wire_profile 值无效"))?;
    let tls_profile: String = row.try_get("tls_profile")?;
    ExternalPoolTlsProfile::parse_known(&tls_profile)
        .ok_or_else(|| anyhow::anyhow!("tls_profile 值无效"))?;
    let route_mode: String = row.try_get("route_mode")?;
    let route_mode = ExternalPoolRouteMode::parse_known(&route_mode)
        .ok_or_else(|| anyhow::anyhow!("route_mode 值无效"))?;
    let route_rules_value: serde_json::Value = row
        .try_get("route_rules")
        .map_err(|_| anyhow::anyhow!("route_rules 字段类型无效"))?;
    let route_rules = decode_external_pool_route_rules(route_rules_value, true)?;
    let model_mapping_mode: String = row.try_get("model_mapping_mode")?;
    ExternalPoolModelMappingMode::parse_known(&model_mapping_mode)
        .ok_or_else(|| anyhow::anyhow!("model_mapping_mode 值无效"))?;
    row.try_get::<bool, _>("model_mapping_require_match")?;
    let model_mapping_rules_value: serde_json::Value = row.try_get("model_mapping_rules")?;
    let model_mapping_rules =
        serde_json::from_value::<Vec<ModelMappingRule>>(model_mapping_rules_value)
            .map_err(|_| anyhow::anyhow!("model_mapping_rules 必须是有效规则数组"))?;
    if model_mapping_rules
        .iter()
        .any(|rule| rule.source.trim().is_empty() || rule.target.trim().is_empty())
    {
        anyhow::bail!("model_mapping_rules 不能包含空 source/target");
    }
    let supported_models_value: serde_json::Value = row
        .try_get("supported_models")
        .map_err(|_| anyhow::anyhow!("supported_models 字段类型无效"))?;
    let supported_models = serde_json::from_value::<Vec<String>>(supported_models_value)
        .map_err(|_| anyhow::anyhow!("supported_models 必须是字符串数组"))?;
    if supported_models.iter().any(|model| model.trim().is_empty()) {
        anyhow::bail!("supported_models 不能包含空模型名");
    }
    Ok(ExternalPoolEligibility {
        id: row.try_get::<i64, _>("id")?.max(0) as u64,
        enabled: row.try_get("enabled")?,
        auto_disabled: row.try_get("auto_disabled")?,
        auto_disabled_until: row.try_get("auto_disabled_until")?,
        supported_models: Arc::new(
            normalize_supported_models(supported_models)
                .into_iter()
                .collect::<HashSet<_>>(),
        ),
        route_mode,
        route_rules: Arc::new(route_rules),
    })
}

fn decode_external_pool_route_rules(
    value: serde_json::Value,
    strict_dispatch: bool,
) -> anyhow::Result<Vec<String>> {
    let rules = if strict_dispatch {
        serde_json::from_value::<Vec<String>>(value)
            .map_err(|_| anyhow::anyhow!("route_rules 必须是字符串数组"))?
    } else {
        serde_json::from_value::<Vec<String>>(value).unwrap_or_default()
    };
    validate_external_pool_route_rules(&rules)?;
    Ok(normalize_route_rules(&rules))
}

fn validate_external_pool_route_rules(rules: &[String]) -> anyhow::Result<()> {
    if rules.len() > 200 {
        anyhow::bail!("route_rules 不能超过 200 条");
    }
    if rules.iter().any(|rule| rule.len() > 256) {
        anyhow::bail!("route_rules 单条长度不能超过 256");
    }
    Ok(())
}

fn external_pool_from_row(row: PgRow, mask_secrets: bool) -> anyhow::Result<ExternalPool> {
    external_pool_from_row_with_policy(row, mask_secrets, false)
}

fn external_pool_from_row_with_policy(
    row: PgRow,
    mask_secrets: bool,
    strict_dispatch: bool,
) -> anyhow::Result<ExternalPool> {
    let id: i64 = row.try_get("id")?;
    let revision: i64 = row.try_get("revision")?;
    let api_key: String = row.try_get("api_key")?;
    let auth_type: String = row.try_get("auth_type")?;
    let usage_projection_mode: String = row.try_get("usage_projection_mode")?;
    let stream_response_mode: Option<String> = if strict_dispatch {
        row.try_get("stream_response_mode")?
    } else {
        row.try_get("stream_response_mode").unwrap_or(None)
    };
    let request_body_mode: String = if strict_dispatch {
        row.try_get("request_body_mode")?
    } else {
        row.try_get("request_body_mode")
            .unwrap_or_else(|_| "normalized".to_string())
    };
    let raw_model_mode: String = if strict_dispatch {
        row.try_get("raw_model_mode")?
    } else {
        row.try_get("raw_model_mode")
            .unwrap_or_else(|_| "none".to_string())
    };
    let auto_disable_policy: String = row.try_get("auto_disable_policy")?;
    let pre_output_stream_retry_mode: String = if strict_dispatch {
        row.try_get("pre_output_stream_retry_mode")?
    } else {
        row.try_get("pre_output_stream_retry_mode")
            .unwrap_or_else(|_| "inherit".to_string())
    };
    let header_profile: String = if strict_dispatch {
        row.try_get("header_profile")?
    } else {
        row.try_get("header_profile")
            .unwrap_or_else(|_| "generic".to_string())
    };
    let append_beta_query: bool = if strict_dispatch {
        row.try_get("append_beta_query")?
    } else {
        row.try_get("append_beta_query").unwrap_or(false)
    };
    let header_overrides_value: serde_json::Value = if strict_dispatch {
        row.try_get("header_overrides")?
    } else {
        row.try_get("header_overrides")
            .unwrap_or_else(|_| serde_json::Value::Object(Default::default()))
    };
    let header_overrides = if strict_dispatch {
        let overrides = serde_json::from_value::<HashMap<String, String>>(header_overrides_value)
            .map_err(|_| anyhow::anyhow!("header_overrides 必须是对象"))?;
        normalize_external_pool_header_overrides(overrides)?
    } else {
        serde_json::from_value::<HashMap<String, String>>(header_overrides_value)
            .ok()
            .and_then(|overrides| normalize_external_pool_header_overrides(overrides).ok())
            .unwrap_or_default()
    };
    let wire_profile: String = if strict_dispatch {
        row.try_get("wire_profile")?
    } else {
        row.try_get("wire_profile")
            .unwrap_or_else(|_| "default".to_string())
    };
    let tls_profile: String = if strict_dispatch {
        row.try_get("tls_profile")?
    } else {
        row.try_get("tls_profile")
            .unwrap_or_else(|_| "default".to_string())
    };
    let route_mode: String = if strict_dispatch {
        row.try_get("route_mode")?
    } else {
        row.try_get("route_mode")
            .unwrap_or_else(|_| "allow_all".to_string())
    };
    let route_rules_value: serde_json::Value = if strict_dispatch {
        row.try_get("route_rules")?
    } else {
        row.try_get("route_rules")
            .unwrap_or_else(|_| serde_json::Value::Array(Vec::new()))
    };
    let route_rules = decode_external_pool_route_rules(route_rules_value, strict_dispatch)?;
    let max_concurrent_requests: i32 = row.try_get("max_concurrent_requests")?;
    let model_mapping_mode: String = row.try_get("model_mapping_mode")?;
    let model_mapping_rules_value: serde_json::Value = row.try_get("model_mapping_rules")?;
    let model_mapping_rules = if strict_dispatch {
        let rules = serde_json::from_value::<Vec<ModelMappingRule>>(model_mapping_rules_value)
            .map_err(|_| anyhow::anyhow!("model_mapping_rules 必须是有效规则数组"))?;
        if rules
            .iter()
            .any(|rule| rule.source.trim().is_empty() || rule.target.trim().is_empty())
        {
            anyhow::bail!("model_mapping_rules 不能包含空 source/target");
        }
        rules
    } else {
        serde_json::from_value::<Vec<ModelMappingRule>>(model_mapping_rules_value)
            .unwrap_or_default()
    };
    let supported_models_value: serde_json::Value = if strict_dispatch {
        row.try_get("supported_models")?
    } else {
        row.try_get("supported_models")
            .unwrap_or_else(|_| serde_json::Value::Array(Vec::new()))
    };
    let supported_models = normalize_supported_models(if strict_dispatch {
        let models = serde_json::from_value::<Vec<String>>(supported_models_value)
            .map_err(|_| anyhow::anyhow!("supported_models 必须是字符串数组"))?;
        if models.iter().any(|model| model.trim().is_empty()) {
            anyhow::bail!("supported_models 不能包含空模型名");
        }
        models
    } else {
        serde_json::from_value::<Vec<String>>(supported_models_value).unwrap_or_default()
    });

    let auth_type = if strict_dispatch {
        ExternalPoolAuthType::parse_known(&auth_type)
            .ok_or_else(|| anyhow::anyhow!("auth_type 值无效"))?
    } else {
        ExternalPoolAuthType::parse(&auth_type)
    };
    let usage_projection_mode = if strict_dispatch {
        ExternalPoolUsageProjectionMode::parse_known(&usage_projection_mode)
            .ok_or_else(|| anyhow::anyhow!("usage_projection_mode 值无效"))?
    } else {
        ExternalPoolUsageProjectionMode::parse(&usage_projection_mode)
    };
    let stream_response_mode = match stream_response_mode.as_deref() {
        Some(value) if strict_dispatch => Some(
            ExternalPoolStreamResponseMode::parse_known(value)
                .ok_or_else(|| anyhow::anyhow!("stream_response_mode 值无效"))?,
        ),
        Some(value) => Some(ExternalPoolStreamResponseMode::parse(value)),
        None => None,
    };
    let request_body_mode = if strict_dispatch {
        ExternalPoolRequestBodyMode::parse_known(&request_body_mode)
            .ok_or_else(|| anyhow::anyhow!("request_body_mode 值无效"))?
    } else {
        ExternalPoolRequestBodyMode::parse(&request_body_mode)
    };
    let raw_model_mode = if strict_dispatch {
        ExternalPoolRawModelMode::parse_known(&raw_model_mode)
            .ok_or_else(|| anyhow::anyhow!("raw_model_mode 值无效"))?
    } else {
        ExternalPoolRawModelMode::parse(&raw_model_mode)
    };
    let auto_disable_policy = if strict_dispatch {
        ExternalPoolAutoDisablePolicy::parse_known(&auto_disable_policy)
            .ok_or_else(|| anyhow::anyhow!("auto_disable_policy 值无效"))?
    } else {
        ExternalPoolAutoDisablePolicy::parse(&auto_disable_policy)
    };
    let pre_output_stream_retry_mode = if strict_dispatch {
        ExternalPoolStreamRetryMode::parse_known(&pre_output_stream_retry_mode)
            .ok_or_else(|| anyhow::anyhow!("pre_output_stream_retry_mode 值无效"))?
    } else {
        ExternalPoolStreamRetryMode::parse(&pre_output_stream_retry_mode)
    };
    let header_profile = if strict_dispatch {
        ExternalPoolHeaderProfile::parse_known(&header_profile)
            .ok_or_else(|| anyhow::anyhow!("header_profile 值无效"))?
    } else {
        ExternalPoolHeaderProfile::parse(&header_profile)
    };
    let wire_profile = if strict_dispatch {
        ExternalPoolWireProfile::parse_known(&wire_profile)
            .ok_or_else(|| anyhow::anyhow!("wire_profile 值无效"))?
    } else {
        ExternalPoolWireProfile::parse(&wire_profile)
    };
    let tls_profile = if strict_dispatch {
        ExternalPoolTlsProfile::parse_known(&tls_profile)
            .ok_or_else(|| anyhow::anyhow!("tls_profile 值无效"))?
    } else {
        ExternalPoolTlsProfile::parse(&tls_profile)
    };
    let route_mode = if strict_dispatch {
        ExternalPoolRouteMode::parse_known(&route_mode)
            .ok_or_else(|| anyhow::anyhow!("route_mode 值无效"))?
    } else {
        ExternalPoolRouteMode::parse(&route_mode)
    };
    let model_mapping_mode = if strict_dispatch {
        ExternalPoolModelMappingMode::parse_known(&model_mapping_mode)
            .ok_or_else(|| anyhow::anyhow!("model_mapping_mode 值无效"))?
    } else {
        ExternalPoolModelMappingMode::parse(&model_mapping_mode)
    };
    let model_mapping_require_match = if strict_dispatch {
        row.try_get("model_mapping_require_match")?
    } else {
        row.try_get("model_mapping_require_match").unwrap_or(false)
    };
    if strict_dispatch {
        if id <= 0 {
            anyhow::bail!("id 必须为正数");
        }
        if revision <= 0 {
            anyhow::bail!("revision 必须为正数");
        }
        if max_concurrent_requests <= 0 {
            anyhow::bail!("max_concurrent_requests 必须为正数");
        }
        if api_key.trim().is_empty() {
            anyhow::bail!("api_key 不能为空");
        }
        let base_url: String = row.try_get("base_url")?;
        let parsed_base_url = url::Url::parse(base_url.trim())
            .map_err(|_| anyhow::anyhow!("base_url 不是有效 URL"))?;
        if !matches!(parsed_base_url.scheme(), "http" | "https") {
            anyhow::bail!("base_url scheme 必须是 http 或 https");
        }
        if parsed_base_url.host_str().is_none() {
            anyhow::bail!("base_url 必须包含 host");
        }
    }
    Ok(ExternalPool {
        id: id.max(0) as u64,
        revision: revision.max(1) as u64,
        name: row.try_get("name")?,
        base_url: row.try_get("base_url")?,
        api_key: (!mask_secrets).then_some(api_key.clone()),
        masked_api_key: Some(mask_external_pool_key(&api_key)),
        auth_type,
        header_profile,
        append_beta_query,
        header_overrides,
        wire_profile,
        tls_profile,
        enabled: row.try_get("enabled")?,
        priority: row.try_get("priority")?,
        max_concurrent_requests: max_concurrent_requests.max(1) as u32,
        usage_projection_mode,
        stream_response_mode,
        request_body_mode,
        raw_model_mode,
        auto_disable_policy,
        pre_output_stream_retry_mode,
        auto_disabled: row.try_get("auto_disabled")?,
        auto_disabled_reason: row.try_get("auto_disabled_reason")?,
        auto_disabled_at: row.try_get("auto_disabled_at")?,
        auto_disabled_until: row.try_get("auto_disabled_until")?,
        auto_disabled_last_error: row.try_get("auto_disabled_last_error")?,
        preserve_path: row.try_get("preserve_path")?,
        normalize_model_version_dots: row.try_get("normalize_model_version_dots")?,
        model_mapping_mode,
        model_mapping_require_match,
        model_mapping_rules: normalize_external_pool_model_mapping_rules(model_mapping_rules),
        supported_models,
        route_mode,
        route_rules,
        notes: row.try_get("notes")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn validate_external_pool_input(
    name: &str,
    base_url: &str,
    max_concurrent_requests: u32,
) -> anyhow::Result<()> {
    if name.trim().is_empty() {
        anyhow::bail!("外部池名称不能为空");
    }
    if base_url.trim().is_empty() {
        anyhow::bail!("外部池 baseUrl 不能为空");
    }
    let parsed = url::Url::parse(base_url.trim())?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        anyhow::bail!("外部池 baseUrl 只支持 http/https");
    }
    if max_concurrent_requests == 0 {
        anyhow::bail!("外部池 maxConcurrentRequests 必须大于 0");
    }
    Ok(())
}

#[allow(dead_code)]
async fn upsert_last_used_at(
    tx: &mut Transaction<'_, Postgres>,
    credential_id: u64,
    last_used_at: &str,
) -> anyhow::Result<()> {
    validate_rfc3339_timestamp("last_used_at", last_used_at)?;
    sqlx::query(
        r#"
        INSERT INTO credential_stats (credential_id, success_count, last_used_at, updated_at)
        VALUES ($1, 0, $2, now())
        ON CONFLICT (credential_id) DO UPDATE
        SET last_used_at = CASE
                WHEN credential_stats.last_used_at IS NULL THEN EXCLUDED.last_used_at
                WHEN EXCLUDED.last_used_at::timestamptz
                     >= credential_stats.last_used_at::timestamptz
                    THEN EXCLUDED.last_used_at
                ELSE credential_stats.last_used_at
            END,
            updated_at = now()
        "#,
    )
    .bind(credential_id as i64)
    .bind(last_used_at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

enum CredentialRuntimeMutationPreparation {
    Apply {
        next_revision: u64,
        current_generation: u64,
        credential_disabled: bool,
    },
    Duplicate {
        state: CredentialRuntimeStateRow,
        credential_disabled: bool,
    },
    Stale {
        state: CredentialRuntimeStateRow,
        credential_disabled: bool,
    },
}

async fn prepare_credential_runtime_mutation(
    tx: &mut Transaction<'_, Postgres>,
    credential_id: u64,
    operation_id: Uuid,
    mutation_kind: &str,
    expected_generation: Option<u64>,
) -> anyhow::Result<CredentialRuntimeMutationPreparation> {
    let operation_id = operation_id.to_string();
    let credential_disabled = lock_active_credential_in_tx(tx, credential_id).await?;
    sqlx::query(
        r#"
        INSERT INTO credential_runtime_state (
            credential_id, failure_count, refresh_failure_count,
            disabled_reason, warmup_remaining, generation, revision, updated_at
        )
        VALUES ($1, 0, 0, NULL, 0, 0, 0, now())
        ON CONFLICT (credential_id) DO NOTHING
        "#,
    )
    .bind(credential_id as i64)
    .execute(&mut **tx)
    .await?;

    let row = sqlx::query(
        r#"
        SELECT failure_count, refresh_failure_count, disabled_reason,
               warmup_remaining, generation, revision
        FROM credential_runtime_state
        WHERE credential_id = $1
        FOR UPDATE
        "#,
    )
    .bind(credential_id as i64)
    .fetch_one(&mut **tx)
    .await?;
    let state = runtime_state_from_row(&row)?;

    let existing = sqlx::query(
        r#"
        SELECT credential_id, mutation_kind
        FROM credential_runtime_mutations
        WHERE operation_id = $1
        FOR UPDATE
        "#,
    )
    .bind(&operation_id)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(existing) = existing {
        let existing_credential_id: i64 = existing.try_get("credential_id")?;
        let existing_mutation_kind: String = existing.try_get("mutation_kind")?;
        if existing_credential_id != credential_id as i64 || existing_mutation_kind != mutation_kind
        {
            anyhow::bail!(
                "运行态 operation_id {} 已用于凭据 #{} 的 {} 操作",
                operation_id,
                existing_credential_id,
                existing_mutation_kind
            );
        }
        sqlx::query(
            "UPDATE credential_runtime_mutations SET created_at = now() WHERE operation_id = $1",
        )
        .bind(&operation_id)
        .execute(&mut **tx)
        .await?;
        return Ok(CredentialRuntimeMutationPreparation::Duplicate {
            state,
            credential_disabled,
        });
    }

    if let Some(expected_generation) = expected_generation {
        if expected_generation < state.generation {
            return Ok(CredentialRuntimeMutationPreparation::Stale {
                state,
                credential_disabled,
            });
        }
        if expected_generation > state.generation {
            anyhow::bail!(
                "凭据 #{} 运行态 generation 超前：期望 {}，当前 {}",
                credential_id,
                expected_generation,
                state.generation
            );
        }
    }

    let next_revision = state
        .revision
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("凭据运行态 revision 已溢出"))?;
    let next_revision_i64 = i64::try_from(next_revision)
        .map_err(|_| anyhow::anyhow!("凭据运行态 revision 超出 PgSQL BIGINT 范围"))?;

    let inserted_revision: Option<i64> = sqlx::query_scalar(
        r#"
        INSERT INTO credential_runtime_mutations (
            operation_id, credential_id, mutation_kind, applied_revision, created_at
        )
        VALUES ($1, $2, $3, $4, now())
        ON CONFLICT (operation_id) DO NOTHING
        RETURNING applied_revision
        "#,
    )
    .bind(&operation_id)
    .bind(credential_id as i64)
    .bind(mutation_kind)
    .bind(next_revision_i64)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(inserted_revision) = inserted_revision {
        if inserted_revision != next_revision_i64 {
            anyhow::bail!("凭据运行态 mutation revision 写入不一致");
        }
        return Ok(CredentialRuntimeMutationPreparation::Apply {
            next_revision,
            current_generation: state.generation,
            credential_disabled,
        });
    }

    let existing = sqlx::query(
        r#"
        SELECT credential_id, mutation_kind
        FROM credential_runtime_mutations
        WHERE operation_id = $1
        FOR UPDATE
        "#,
    )
    .bind(&operation_id)
    .fetch_one(&mut **tx)
    .await?;
    let existing_credential_id: i64 = existing.try_get("credential_id")?;
    let existing_mutation_kind: String = existing.try_get("mutation_kind")?;
    if existing_credential_id != credential_id as i64 || existing_mutation_kind != mutation_kind {
        anyhow::bail!(
            "运行态 operation_id {} 已用于凭据 #{} 的 {} 操作",
            operation_id,
            existing_credential_id,
            existing_mutation_kind
        );
    }
    sqlx::query(
        "UPDATE credential_runtime_mutations SET created_at = now() WHERE operation_id = $1",
    )
    .bind(&operation_id)
    .execute(&mut **tx)
    .await?;
    Ok(CredentialRuntimeMutationPreparation::Duplicate {
        state,
        credential_disabled,
    })
}

fn verify_runtime_mutation_revision(
    state: &CredentialRuntimeStateRow,
    expected_revision: u64,
) -> anyhow::Result<()> {
    if state.revision != expected_revision {
        anyhow::bail!(
            "凭据运行态 revision 不一致：期望 {}，实际 {}",
            expected_revision,
            state.revision
        );
    }
    Ok(())
}

fn verify_runtime_mutation_generation(
    state: &CredentialRuntimeStateRow,
    expected_generation: u64,
) -> anyhow::Result<()> {
    if state.generation != expected_generation {
        anyhow::bail!(
            "凭据运行态 generation 不一致：期望 {}，实际 {}",
            expected_generation,
            state.generation
        );
    }
    Ok(())
}

fn validate_rfc3339_timestamp(field: &str, value: &str) -> anyhow::Result<()> {
    DateTime::parse_from_rfc3339(value)
        .map(|_| ())
        .map_err(|err| anyhow::anyhow!("{} 必须是有效 RFC3339 时间: {}", field, err))
}

fn validate_credential_runtime_state_patch(
    patch: &CredentialRuntimeStatePatch,
) -> anyhow::Result<()> {
    runtime_state_count_to_i32("failure_count", patch.failure_count)?;
    runtime_state_count_to_i32("refresh_failure_count", patch.refresh_failure_count)?;
    runtime_state_count_to_i32("warmup_remaining", patch.warmup_remaining)?;
    if patch
        .expected_generation
        .is_some_and(|generation| i64::try_from(generation).is_err())
    {
        anyhow::bail!("凭据运行态 expected generation 超出 PgSQL BIGINT 范围");
    }
    if let Some(last_used_at) = patch.last_used_at.as_deref() {
        validate_rfc3339_timestamp("last_used_at", last_used_at)?;
    }
    Ok(())
}

fn runtime_state_count_to_i32(field: &str, value: Option<u32>) -> anyhow::Result<Option<i32>> {
    value
        .map(|value| {
            i32::try_from(value).map_err(|_| {
                anyhow::anyhow!(
                    "凭据运行态字段 {} 的值 {} 超出 PgSQL INTEGER 范围",
                    field,
                    value
                )
            })
        })
        .transpose()
}

async fn lock_active_credential_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    credential_id: u64,
) -> anyhow::Result<bool> {
    sqlx::query_scalar(
        r#"
        SELECT disabled
        FROM credentials
        WHERE id = $1 AND deleted_at IS NULL
        FOR UPDATE
        "#,
    )
    .bind(credential_id as i64)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| anyhow::anyhow!("凭据 #{} 不存在或已删除，无法更新运行态", credential_id))
}

async fn persist_credential_disabled_flag_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    credential_id: u64,
    disabled: bool,
) -> anyhow::Result<()> {
    let result = sqlx::query(
        r#"
        UPDATE credentials
        SET disabled = $2,
            data = jsonb_set(data, '{disabled}', to_jsonb($2::boolean), true),
            updated_at = now(),
            revision = credentials.revision + 1
        WHERE id = $1 AND deleted_at IS NULL
        "#,
    )
    .bind(credential_id as i64)
    .bind(disabled)
    .execute(&mut **tx)
    .await?;
    if result.rows_affected() != 1 {
        anyhow::bail!("凭据 #{} 不存在或已删除，无法更新禁用状态", credential_id);
    }
    Ok(())
}

fn usize_to_i64(value: usize) -> i64 {
    value.min(i64::MAX as usize) as i64
}

fn u64_to_i64(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

fn usage_status_value(status: UsageRecordStatus) -> &'static str {
    match status {
        UsageRecordStatus::Success => "success",
        UsageRecordStatus::Error => "error",
        UsageRecordStatus::StreamError => "stream_error",
        UsageRecordStatus::UpstreamTimeout => "upstream_timeout",
        UsageRecordStatus::ClientDropped => "client_dropped",
    }
}

fn usage_source_value(source: crate::anthropic::usage::UsageSource) -> &'static str {
    match source {
        crate::anthropic::usage::UsageSource::UpstreamMetadata => "upstream_metadata",
        crate::anthropic::usage::UsageSource::LocalPromptCache => "local_prompt_cache",
        crate::anthropic::usage::UsageSource::ContextEstimate => "context_estimate",
        crate::anthropic::usage::UsageSource::RequestEstimate => "request_estimate",
        crate::anthropic::usage::UsageSource::None => "none",
    }
}

fn usage_route_kind_value(route_kind: UsageRouteKind) -> &'static str {
    match route_kind {
        UsageRouteKind::LocalCredential => "local_credential",
        UsageRouteKind::ExternalPool => "external_pool",
    }
}

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS runtime_config (
    id TEXT PRIMARY KEY,
    config JSONB NOT NULL,
    version BIGINT NOT NULL DEFAULT 1,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE runtime_config
    ADD COLUMN IF NOT EXISTS version BIGINT NOT NULL DEFAULT 1;

CREATE SEQUENCE IF NOT EXISTS credentials_id_seq;

CREATE TABLE IF NOT EXISTS credentials (
    id BIGINT PRIMARY KEY DEFAULT nextval('credentials_id_seq'),
    priority INTEGER NOT NULL DEFAULT 0,
    disabled BOOLEAN NOT NULL DEFAULT false,
    auth_kind TEXT NOT NULL DEFAULT 'oauth',
    api_key_hash TEXT,
    refresh_token_hash TEXT,
    data JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    revision BIGINT NOT NULL DEFAULT 1,
    deleted_at TIMESTAMPTZ
);

ALTER TABLE credentials
    ALTER COLUMN id SET DEFAULT nextval('credentials_id_seq');

ALTER TABLE credentials
    ADD COLUMN IF NOT EXISTS auth_kind TEXT NOT NULL DEFAULT 'oauth';

ALTER TABLE credentials
    ADD COLUMN IF NOT EXISTS api_key_hash TEXT;

ALTER TABLE credentials
    ADD COLUMN IF NOT EXISTS refresh_token_hash TEXT;

ALTER TABLE credentials
    ADD COLUMN IF NOT EXISTS created_at TIMESTAMPTZ NOT NULL DEFAULT now();

ALTER TABLE credentials
    ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT now();

ALTER TABLE credentials
    ADD COLUMN IF NOT EXISTS revision BIGINT NOT NULL DEFAULT 1;

ALTER TABLE credentials
    ADD COLUMN IF NOT EXISTS deleted_at TIMESTAMPTZ;

-- 这里的 active 是数据库索引名里的历史术语，含义是“未软删除”
-- (`deleted_at IS NULL`)，不是 `disabled = false`。禁用凭据仍属于后台
-- 管理范围，也继续参与重复导入检测。
CREATE INDEX IF NOT EXISTS idx_credentials_active_priority
    ON credentials (priority, id)
    WHERE deleted_at IS NULL;

CREATE UNIQUE INDEX IF NOT EXISTS uniq_active_credentials_api_key_hash
    ON credentials (api_key_hash)
    WHERE deleted_at IS NULL AND api_key_hash IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS uniq_active_credentials_refresh_token_hash
    ON credentials (refresh_token_hash)
    WHERE deleted_at IS NULL AND refresh_token_hash IS NOT NULL;

CREATE TABLE IF NOT EXISTS proxy_resources (
    id BIGSERIAL PRIMARY KEY,
    name TEXT NOT NULL,
    proxy_url TEXT NOT NULL,
    proxy_username TEXT,
    proxy_password TEXT,
    enabled BOOLEAN NOT NULL DEFAULT true,
    notes TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at TIMESTAMPTZ
);

ALTER TABLE proxy_resources
    ADD COLUMN IF NOT EXISTS proxy_username TEXT;

ALTER TABLE proxy_resources
    ADD COLUMN IF NOT EXISTS proxy_password TEXT;

ALTER TABLE proxy_resources
    ADD COLUMN IF NOT EXISTS enabled BOOLEAN NOT NULL DEFAULT true;

ALTER TABLE proxy_resources
    ADD COLUMN IF NOT EXISTS notes TEXT;

ALTER TABLE proxy_resources
    ADD COLUMN IF NOT EXISTS created_at TIMESTAMPTZ NOT NULL DEFAULT now();

ALTER TABLE proxy_resources
    ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT now();

ALTER TABLE proxy_resources
    ADD COLUMN IF NOT EXISTS deleted_at TIMESTAMPTZ;

CREATE INDEX IF NOT EXISTS idx_proxy_resources_active
    ON proxy_resources (id)
    WHERE deleted_at IS NULL;

CREATE TABLE IF NOT EXISTS external_upstream_pools (
    id BIGSERIAL PRIMARY KEY,
    revision BIGINT NOT NULL DEFAULT 1,
    name TEXT NOT NULL,
    base_url TEXT NOT NULL,
    api_key TEXT NOT NULL,
    auth_type TEXT NOT NULL DEFAULT 'bearer',
    enabled BOOLEAN NOT NULL DEFAULT true,
    priority INTEGER NOT NULL DEFAULT 100,
    max_concurrent_requests INTEGER NOT NULL DEFAULT 10,
    usage_projection_mode TEXT NOT NULL DEFAULT 'pass_through',
    stream_response_mode TEXT,
    skip_non_stream_usage_projection BOOLEAN NOT NULL DEFAULT false,
    request_body_mode TEXT NOT NULL DEFAULT 'normalized',
    raw_model_mode TEXT NOT NULL DEFAULT 'none',
    auto_disable_policy TEXT NOT NULL DEFAULT 'inherit',
    pre_output_stream_retry_mode TEXT NOT NULL DEFAULT 'inherit',
    header_profile TEXT NOT NULL DEFAULT 'generic',
    append_beta_query BOOLEAN NOT NULL DEFAULT false,
    header_overrides JSONB NOT NULL DEFAULT '{}'::jsonb,
    wire_profile TEXT NOT NULL DEFAULT 'default',
    tls_profile TEXT NOT NULL DEFAULT 'default',
    auto_disabled BOOLEAN NOT NULL DEFAULT false,
    auto_disabled_reason TEXT,
    auto_disabled_at TIMESTAMPTZ,
    auto_disabled_until TIMESTAMPTZ,
    auto_disabled_last_error TEXT,
    preserve_path BOOLEAN NOT NULL DEFAULT true,
    normalize_model_version_dots BOOLEAN NOT NULL DEFAULT false,
    model_mapping_mode TEXT NOT NULL DEFAULT 'processed_mapping',
    model_mapping_require_match BOOLEAN NOT NULL DEFAULT false,
    model_mapping_rules JSONB NOT NULL DEFAULT '[]'::jsonb,
    supported_models JSONB NOT NULL DEFAULT '[]'::jsonb,
    route_mode TEXT NOT NULL DEFAULT 'allow_all',
    route_rules JSONB NOT NULL DEFAULT '[]'::jsonb,
    notes TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at TIMESTAMPTZ
);

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS revision BIGINT NOT NULL DEFAULT 1;

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS auth_type TEXT NOT NULL DEFAULT 'bearer';

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS enabled BOOLEAN NOT NULL DEFAULT true;

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS priority INTEGER NOT NULL DEFAULT 100;

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS max_concurrent_requests INTEGER NOT NULL DEFAULT 10;

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS usage_projection_mode TEXT NOT NULL DEFAULT 'pass_through';

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS stream_response_mode TEXT;

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS skip_non_stream_usage_projection BOOLEAN NOT NULL DEFAULT false;

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS request_body_mode TEXT NOT NULL DEFAULT 'normalized';

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS raw_model_mode TEXT NOT NULL DEFAULT 'none';

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS auto_disable_policy TEXT NOT NULL DEFAULT 'inherit';

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS pre_output_stream_retry_mode TEXT NOT NULL DEFAULT 'inherit';

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS header_profile TEXT NOT NULL DEFAULT 'generic';

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS append_beta_query BOOLEAN NOT NULL DEFAULT false;

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS header_overrides JSONB NOT NULL DEFAULT '{}'::jsonb;

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS wire_profile TEXT NOT NULL DEFAULT 'default';

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS tls_profile TEXT NOT NULL DEFAULT 'default';

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS auto_disabled BOOLEAN NOT NULL DEFAULT false;

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS auto_disabled_reason TEXT;

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS auto_disabled_at TIMESTAMPTZ;

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS auto_disabled_until TIMESTAMPTZ;

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS auto_disabled_last_error TEXT;

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS preserve_path BOOLEAN NOT NULL DEFAULT true;

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS normalize_model_version_dots BOOLEAN NOT NULL DEFAULT false;

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS model_mapping_mode TEXT NOT NULL DEFAULT 'processed_mapping';

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS model_mapping_require_match BOOLEAN NOT NULL DEFAULT false;

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS model_mapping_rules JSONB NOT NULL DEFAULT '[]'::jsonb;

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS supported_models JSONB NOT NULL DEFAULT '[]'::jsonb;

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS route_mode TEXT NOT NULL DEFAULT 'allow_all';

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS route_rules JSONB NOT NULL DEFAULT '[]'::jsonb;

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS notes TEXT;

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS created_at TIMESTAMPTZ NOT NULL DEFAULT now();

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT now();

ALTER TABLE external_upstream_pools
    ADD COLUMN IF NOT EXISTS deleted_at TIMESTAMPTZ;

CREATE INDEX IF NOT EXISTS idx_external_upstream_pools_active_priority
    ON external_upstream_pools (priority ASC, id ASC)
    WHERE deleted_at IS NULL AND enabled = true;

CREATE INDEX IF NOT EXISTS idx_external_upstream_pools_auto_disabled
    ON external_upstream_pools (auto_disabled, auto_disabled_until)
    WHERE deleted_at IS NULL;

CREATE TABLE IF NOT EXISTS credential_stats (
    credential_id BIGINT PRIMARY KEY REFERENCES credentials(id) ON DELETE CASCADE,
    success_count BIGINT NOT NULL DEFAULT 0,
    selection_count BIGINT NOT NULL DEFAULT 0,
    last_used_at TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS credential_stats_delta_batches (
    operation_id TEXT PRIMARY KEY,
    payload_hash TEXT NOT NULL,
    input_credential_count INTEGER NOT NULL,
    applied_credential_count INTEGER NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_credential_stats_delta_batches_created_at
    ON credential_stats_delta_batches (created_at ASC);

ALTER TABLE credential_stats
    ADD COLUMN IF NOT EXISTS selection_count BIGINT NOT NULL DEFAULT 0;

CREATE TABLE IF NOT EXISTS credential_runtime_state (
    credential_id BIGINT PRIMARY KEY REFERENCES credentials(id) ON DELETE CASCADE,
    failure_count INTEGER NOT NULL DEFAULT 0,
    refresh_failure_count INTEGER NOT NULL DEFAULT 0,
    disabled_reason TEXT,
    warmup_remaining INTEGER NOT NULL DEFAULT 0,
    generation BIGINT NOT NULL DEFAULT 0,
    revision BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE credential_runtime_state
    ADD COLUMN IF NOT EXISTS revision BIGINT NOT NULL DEFAULT 0;

ALTER TABLE credential_runtime_state
    ADD COLUMN IF NOT EXISTS generation BIGINT NOT NULL DEFAULT 0;

CREATE TABLE IF NOT EXISTS credential_runtime_mutations (
    operation_id TEXT PRIMARY KEY,
    credential_id BIGINT NOT NULL REFERENCES credential_runtime_state(credential_id) ON DELETE CASCADE,
    mutation_kind TEXT NOT NULL,
    applied_revision BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_credential_runtime_mutations_credential_created
    ON credential_runtime_mutations (credential_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_credential_runtime_mutations_created_at
    ON credential_runtime_mutations (created_at ASC);

CREATE TABLE IF NOT EXISTS credential_account_info (
    credential_id BIGINT PRIMARY KEY REFERENCES credentials(id) ON DELETE CASCADE,
    subscription_title TEXT,
    current_usage DOUBLE PRECISION NOT NULL DEFAULT 0,
    usage_limit DOUBLE PRECISION NOT NULL DEFAULT 0,
    remaining DOUBLE PRECISION NOT NULL DEFAULT 0,
    usage_percentage DOUBLE PRECISION NOT NULL DEFAULT 0,
    credit_limit DOUBLE PRECISION NOT NULL DEFAULT 0,
    credit_remaining DOUBLE PRECISION NOT NULL DEFAULT 0,
    credit_base DOUBLE PRECISION NOT NULL DEFAULT 0,
    credit_bonus DOUBLE PRECISION NOT NULL DEFAULT 0,
    overage_status TEXT,
    overage_capability TEXT,
    overage_cap DOUBLE PRECISION NOT NULL DEFAULT 0,
    overage_rate DOUBLE PRECISION NOT NULL DEFAULT 0,
    current_overages DOUBLE PRECISION NOT NULL DEFAULT 0,
    next_reset_at DOUBLE PRECISION,
    checked_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE credential_account_info
    ADD COLUMN IF NOT EXISTS credit_limit DOUBLE PRECISION NOT NULL DEFAULT 0;
ALTER TABLE credential_account_info
    ADD COLUMN IF NOT EXISTS credit_remaining DOUBLE PRECISION NOT NULL DEFAULT 0;
ALTER TABLE credential_account_info
    ADD COLUMN IF NOT EXISTS credit_base DOUBLE PRECISION NOT NULL DEFAULT 0;
ALTER TABLE credential_account_info
    ADD COLUMN IF NOT EXISTS credit_bonus DOUBLE PRECISION NOT NULL DEFAULT 0;
ALTER TABLE credential_account_info
    ADD COLUMN IF NOT EXISTS overage_status TEXT;
ALTER TABLE credential_account_info
    ADD COLUMN IF NOT EXISTS overage_capability TEXT;
ALTER TABLE credential_account_info
    ADD COLUMN IF NOT EXISTS overage_cap DOUBLE PRECISION NOT NULL DEFAULT 0;
ALTER TABLE credential_account_info
    ADD COLUMN IF NOT EXISTS overage_rate DOUBLE PRECISION NOT NULL DEFAULT 0;
ALTER TABLE credential_account_info
    ADD COLUMN IF NOT EXISTS current_overages DOUBLE PRECISION NOT NULL DEFAULT 0;

CREATE TABLE IF NOT EXISTS usage_records (
    id TEXT PRIMARY KEY,
    created_at TIMESTAMPTZ NOT NULL,
    endpoint TEXT NOT NULL,
    stream BOOLEAN NOT NULL,
    model TEXT NOT NULL,
    conversation_id TEXT,
    credential_id BIGINT,
    credential_label TEXT,
    status TEXT NOT NULL,
    usage_source TEXT NOT NULL,
    total_input_tokens INTEGER NOT NULL,
    compat_input_tokens INTEGER NOT NULL,
    billable_input_tokens INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    cache_read_input_tokens INTEGER NOT NULL,
    cache_creation_input_tokens INTEGER NOT NULL,
    cache_creation_5m_input_tokens INTEGER NOT NULL,
    cache_creation_1h_input_tokens INTEGER NOT NULL,
    estimated_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    original_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    kiro_metering_usage DOUBLE PRECISION NOT NULL DEFAULT 0,
    pricing_available BOOLEAN NOT NULL DEFAULT false,
    pricing_model TEXT,
    duration_ms BIGINT NOT NULL DEFAULT 0,
    simulated BOOLEAN NOT NULL DEFAULT false,
    sticky_bound BOOLEAN NOT NULL DEFAULT false,
    fallback_from_sticky BOOLEAN NOT NULL DEFAULT false,
    error_type TEXT,
    error_message TEXT,
    error_detail TEXT,
    data JSONB NOT NULL,
    deleted_at TIMESTAMPTZ,
    rollup_active BOOLEAN NOT NULL DEFAULT true,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE usage_records
    ADD COLUMN IF NOT EXISTS endpoint TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS stream BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS model TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS conversation_id TEXT,
    ADD COLUMN IF NOT EXISTS credential_id BIGINT,
    ADD COLUMN IF NOT EXISTS credential_label TEXT,
    ADD COLUMN IF NOT EXISTS status TEXT NOT NULL DEFAULT 'unknown',
    ADD COLUMN IF NOT EXISTS usage_source TEXT NOT NULL DEFAULT 'none',
    ADD COLUMN IF NOT EXISTS total_input_tokens INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS compat_input_tokens INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS billable_input_tokens INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS output_tokens INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS cache_read_input_tokens INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS cache_creation_input_tokens INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS cache_creation_5m_input_tokens INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS cache_creation_1h_input_tokens INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS estimated_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS original_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS kiro_metering_usage DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS pricing_available BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS pricing_model TEXT,
    ADD COLUMN IF NOT EXISTS duration_ms BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS simulated BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS sticky_bound BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS fallback_from_sticky BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS error_type TEXT,
    ADD COLUMN IF NOT EXISTS error_message TEXT,
    ADD COLUMN IF NOT EXISTS error_detail TEXT,
    ADD COLUMN IF NOT EXISTS data JSONB NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN IF NOT EXISTS deleted_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS rollup_active BOOLEAN NOT NULL DEFAULT true,
    ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT now();

CREATE TABLE IF NOT EXISTS usage_cleanup_watermarks (
    scope TEXT PRIMARY KEY,
    cutoff_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS usage_cleanup_jobs (
    job_id TEXT PRIMARY KEY,
    mode TEXT NOT NULL CHECK (mode IN ('soft_delete', 'hard_delete')),
    cutoff_at TIMESTAMPTZ NOT NULL,
    batch_size INTEGER NOT NULL CHECK (batch_size > 0 AND batch_size <= 5000),
    max_batches INTEGER NOT NULL CHECK (max_batches > 0 AND max_batches <= 10000),
    pause_ms_between_batches BIGINT NOT NULL CHECK (
        pause_ms_between_batches >= 0 AND pause_ms_between_batches <= 10000
    ),
    status TEXT NOT NULL CHECK (
        status IN ('queued', 'running', 'paused', 'completed', 'cancelled', 'failed')
    ),
    phase TEXT NOT NULL DEFAULT 'postgres' CHECK (
        phase IN ('postgres', 'redis_admin_cache', 'redis_snapshots', 'complete')
    ),
    matched_rows BIGINT,
    remaining_rows BIGINT,
    processed_rows BIGINT NOT NULL DEFAULT 0,
    last_batch_rows BIGINT NOT NULL DEFAULT 0,
    batches INTEGER NOT NULL DEFAULT 0,
    cancel_requested BOOLEAN NOT NULL DEFAULT false,
    stop_reason TEXT,
    last_error TEXT,
    redis_deleted_keys BIGINT NOT NULL DEFAULT 0,
    redis_delete_commands BIGINT NOT NULL DEFAULT 0,
    redis_max_command_keys INTEGER NOT NULL DEFAULT 0,
    redis_scan_passes INTEGER NOT NULL DEFAULT 0,
    redis_used_del_fallback BOOLEAN NOT NULL DEFAULT false,
    redis_pass_limit_reached BOOLEAN NOT NULL DEFAULT false,
    lease_owner TEXT,
    lease_until TIMESTAMPTZ,
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_usage_cleanup_jobs_single_active
    ON usage_cleanup_jobs ((1))
    WHERE status IN ('queued', 'running');
CREATE INDEX IF NOT EXISTS idx_usage_cleanup_jobs_started
    ON usage_cleanup_jobs (started_at DESC, job_id DESC);
CREATE INDEX IF NOT EXISTS idx_usage_cleanup_jobs_recovery
    ON usage_cleanup_jobs (status, lease_until, started_at)
    WHERE status IN ('queued', 'running');

CREATE TABLE IF NOT EXISTS usage_rollup_totals (
    dimension TEXT NOT NULL,
    dimension_key TEXT NOT NULL,
    dimension_label TEXT,
    requests BIGINT NOT NULL DEFAULT 0,
    success_requests BIGINT NOT NULL DEFAULT 0,
    error_requests BIGINT NOT NULL DEFAULT 0,
    stream_requests BIGINT NOT NULL DEFAULT 0,
    non_stream_requests BIGINT NOT NULL DEFAULT 0,
    priced_requests BIGINT NOT NULL DEFAULT 0,
    unpriced_requests BIGINT NOT NULL DEFAULT 0,
    local_prompt_cache_requests BIGINT NOT NULL DEFAULT 0,
    simulated_requests BIGINT NOT NULL DEFAULT 0,
    upstream_metadata_requests BIGINT NOT NULL DEFAULT 0,
    sticky_bound_requests BIGINT NOT NULL DEFAULT 0,
    fallback_from_sticky_requests BIGINT NOT NULL DEFAULT 0,
    total_input_tokens BIGINT NOT NULL DEFAULT 0,
    billable_input_tokens BIGINT NOT NULL DEFAULT 0,
    total_output_tokens BIGINT NOT NULL DEFAULT 0,
    total_cache_read_input_tokens BIGINT NOT NULL DEFAULT 0,
    total_cache_creation_input_tokens BIGINT NOT NULL DEFAULT 0,
    local_prompt_cache_input_tokens BIGINT NOT NULL DEFAULT 0,
    local_prompt_cache_read_input_tokens BIGINT NOT NULL DEFAULT 0,
    local_prompt_cache_creation_input_tokens BIGINT NOT NULL DEFAULT 0,
    total_estimated_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    total_original_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    total_kiro_metering_usage DOUBLE PRECISION NOT NULL DEFAULT 0,
    external_pool_requests BIGINT NOT NULL DEFAULT 0,
    external_pool_priced_requests BIGINT NOT NULL DEFAULT 0,
    external_pool_unpriced_requests BIGINT NOT NULL DEFAULT 0,
    external_pool_cost_floor_applied_requests BIGINT NOT NULL DEFAULT 0,
    external_pool_raw_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    external_pool_shaped_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    external_pool_uplifted_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    external_pool_profit_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    external_pool_reported_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    external_pool_billable_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    external_pool_cost_floor_delta_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    duration_ms_sum BIGINT NOT NULL DEFAULT 0,
    duration_ms_count BIGINT NOT NULL DEFAULT 0,
    duration_ms_max BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (dimension, dimension_key)
);

ALTER TABLE usage_rollup_totals
    ADD COLUMN IF NOT EXISTS dimension_label TEXT,
    ADD COLUMN IF NOT EXISTS requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS success_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS error_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS stream_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS non_stream_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS priced_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS unpriced_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS local_prompt_cache_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS simulated_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS upstream_metadata_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS sticky_bound_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS fallback_from_sticky_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS total_input_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS billable_input_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS total_output_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS total_cache_read_input_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS total_cache_creation_input_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS local_prompt_cache_input_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS local_prompt_cache_read_input_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS local_prompt_cache_creation_input_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS total_estimated_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS total_original_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS total_kiro_metering_usage DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS external_pool_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS external_pool_priced_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS external_pool_unpriced_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS external_pool_cost_floor_applied_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS external_pool_raw_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS external_pool_shaped_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS external_pool_uplifted_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS external_pool_profit_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS external_pool_reported_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS external_pool_billable_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS external_pool_cost_floor_delta_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS duration_ms_sum BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS duration_ms_count BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS duration_ms_max BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT now();

CREATE TABLE IF NOT EXISTS usage_rollup_time_buckets (
    bucket_start TIMESTAMPTZ NOT NULL,
    dimension TEXT NOT NULL,
    dimension_key TEXT NOT NULL,
    dimension_label TEXT,
    requests BIGINT NOT NULL DEFAULT 0,
    success_requests BIGINT NOT NULL DEFAULT 0,
    error_requests BIGINT NOT NULL DEFAULT 0,
    stream_requests BIGINT NOT NULL DEFAULT 0,
    non_stream_requests BIGINT NOT NULL DEFAULT 0,
    priced_requests BIGINT NOT NULL DEFAULT 0,
    unpriced_requests BIGINT NOT NULL DEFAULT 0,
    local_prompt_cache_requests BIGINT NOT NULL DEFAULT 0,
    simulated_requests BIGINT NOT NULL DEFAULT 0,
    upstream_metadata_requests BIGINT NOT NULL DEFAULT 0,
    sticky_bound_requests BIGINT NOT NULL DEFAULT 0,
    fallback_from_sticky_requests BIGINT NOT NULL DEFAULT 0,
    total_input_tokens BIGINT NOT NULL DEFAULT 0,
    billable_input_tokens BIGINT NOT NULL DEFAULT 0,
    total_output_tokens BIGINT NOT NULL DEFAULT 0,
    total_cache_read_input_tokens BIGINT NOT NULL DEFAULT 0,
    total_cache_creation_input_tokens BIGINT NOT NULL DEFAULT 0,
    local_prompt_cache_input_tokens BIGINT NOT NULL DEFAULT 0,
    local_prompt_cache_read_input_tokens BIGINT NOT NULL DEFAULT 0,
    local_prompt_cache_creation_input_tokens BIGINT NOT NULL DEFAULT 0,
    total_estimated_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    total_original_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    total_kiro_metering_usage DOUBLE PRECISION NOT NULL DEFAULT 0,
    external_pool_requests BIGINT NOT NULL DEFAULT 0,
    external_pool_priced_requests BIGINT NOT NULL DEFAULT 0,
    external_pool_unpriced_requests BIGINT NOT NULL DEFAULT 0,
    external_pool_cost_floor_applied_requests BIGINT NOT NULL DEFAULT 0,
    external_pool_raw_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    external_pool_shaped_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    external_pool_uplifted_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    external_pool_profit_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    external_pool_reported_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    external_pool_billable_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    external_pool_cost_floor_delta_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    duration_ms_sum BIGINT NOT NULL DEFAULT 0,
    duration_ms_count BIGINT NOT NULL DEFAULT 0,
    duration_ms_max BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (bucket_start, dimension, dimension_key)
);

ALTER TABLE usage_rollup_time_buckets
    ADD COLUMN IF NOT EXISTS dimension_label TEXT,
    ADD COLUMN IF NOT EXISTS requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS success_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS error_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS stream_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS non_stream_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS priced_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS unpriced_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS local_prompt_cache_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS simulated_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS upstream_metadata_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS sticky_bound_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS fallback_from_sticky_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS total_input_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS billable_input_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS total_output_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS total_cache_read_input_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS total_cache_creation_input_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS local_prompt_cache_input_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS local_prompt_cache_read_input_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS local_prompt_cache_creation_input_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS total_estimated_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS total_original_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS total_kiro_metering_usage DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS external_pool_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS external_pool_priced_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS external_pool_unpriced_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS external_pool_cost_floor_applied_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS external_pool_raw_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS external_pool_shaped_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS external_pool_uplifted_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS external_pool_profit_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS external_pool_reported_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS external_pool_billable_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS external_pool_cost_floor_delta_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS duration_ms_sum BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS duration_ms_count BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS duration_ms_max BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT now();

CREATE TABLE IF NOT EXISTS usage_cache_read_totals (
    cache_read_input_tokens INTEGER NOT NULL PRIMARY KEY,
    requests BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS usage_cache_read_rollup_time_buckets (
    bucket_start TIMESTAMPTZ NOT NULL,
    cache_read_input_tokens INTEGER NOT NULL,
    requests BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (bucket_start, cache_read_input_tokens)
);

CREATE TABLE IF NOT EXISTS usage_duration_rollup_time_buckets (
    bucket_start TIMESTAMPTZ NOT NULL,
    duration_ms INTEGER NOT NULL,
    requests BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (bucket_start, duration_ms)
);

CREATE TABLE IF NOT EXISTS usage_credential_cost_summary (
    credential_id BIGINT NOT NULL PRIMARY KEY,
    requests BIGINT NOT NULL DEFAULT 0,
    estimated_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    original_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    kiro_metering_usage DOUBLE PRECISION NOT NULL DEFAULT 0,
    priced_requests BIGINT NOT NULL DEFAULT 0,
    unpriced_requests BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE usage_credential_cost_summary
    ADD COLUMN IF NOT EXISTS requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS estimated_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS original_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS kiro_metering_usage DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS priced_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS unpriced_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT now();

CREATE TABLE IF NOT EXISTS model_pricing (
    model TEXT PRIMARY KEY,
    input_cost_per_token DOUBLE PRECISION NOT NULL,
    output_cost_per_token DOUBLE PRECISION NOT NULL,
    cache_creation_input_token_cost DOUBLE PRECISION NOT NULL,
    cache_read_input_token_cost DOUBLE PRECISION NOT NULL,
    source TEXT NOT NULL,
    source_url TEXT NOT NULL,
    last_synced_at TEXT,
    last_error TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS model_pricing_sync_status (
    id TEXT PRIMARY KEY,
    source TEXT NOT NULL,
    source_url TEXT NOT NULL,
    last_synced_at TEXT,
    last_error TEXT,
    model_count INTEGER NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS model_capabilities (
    model TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    description TEXT,
    max_input_tokens INTEGER,
    max_output_tokens INTEGER,
    supports_prompt_caching BOOLEAN,
    supported_input_types TEXT[] NOT NULL DEFAULT '{}',
    source TEXT NOT NULL,
    last_synced_at TEXT,
    last_error TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS model_capabilities_sync_status (
    id TEXT PRIMARY KEY,
    source TEXT NOT NULL,
    last_synced_at TEXT,
    last_error TEXT,
    model_count INTEGER NOT NULL DEFAULT 0,
    reasoning_fields JSONB NOT NULL DEFAULT '{}'::jsonb,
    reasoning_cohort_keys JSONB NOT NULL DEFAULT '[]'::jsonb,
    reasoning_cohort_complete BOOLEAN NOT NULL DEFAULT FALSE,
    reasoning_contract_version INTEGER NOT NULL DEFAULT 0,
    reasoning_invalid_models JSONB NOT NULL DEFAULT '[]'::jsonb,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE model_capabilities_sync_status
    ADD COLUMN IF NOT EXISTS reasoning_fields JSONB NOT NULL DEFAULT '{}'::jsonb;
ALTER TABLE model_capabilities_sync_status
    ADD COLUMN IF NOT EXISTS reasoning_cohort_keys JSONB NOT NULL DEFAULT '[]'::jsonb;
ALTER TABLE model_capabilities_sync_status
    ADD COLUMN IF NOT EXISTS reasoning_cohort_complete BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE model_capabilities_sync_status
    ADD COLUMN IF NOT EXISTS reasoning_contract_version INTEGER NOT NULL DEFAULT 0;
ALTER TABLE model_capabilities_sync_status
    ADD COLUMN IF NOT EXISTS reasoning_invalid_models JSONB NOT NULL DEFAULT '[]'::jsonb;

CREATE TABLE IF NOT EXISTS admin_audit_logs (
    id BIGSERIAL PRIMARY KEY,
    actor TEXT NOT NULL,
    action TEXT NOT NULL,
    object_type TEXT NOT NULL,
    object_id TEXT,
    success BOOLEAN NOT NULL,
    error_message TEXT,
    detail JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_admin_audit_logs_created_at
    ON admin_audit_logs (created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_admin_audit_logs_action_created
    ON admin_audit_logs (action, created_at DESC);

CREATE TABLE IF NOT EXISTS credential_events (
    id BIGSERIAL PRIMARY KEY,
    credential_id BIGINT REFERENCES credentials(id) ON DELETE SET NULL,
    event_type TEXT NOT NULL,
    reason TEXT,
    detail JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_credential_events_credential_created
    ON credential_events (credential_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_credential_events_type_created
    ON credential_events (event_type, created_at DESC);
"#;

// Explicit maintenance-only backfill. Do not call this from the default startup
// migration path; it can scan/update historical usage tables on large installs.
const USAGE_LEGACY_COST_FIELD_BACKFILL_SQL: &str = r#"
UPDATE usage_records
SET original_cost_usd = COALESCE(
    NULLIF(data #>> '{externalPoolBilling,rawCostUsd}', '')::double precision,
    NULLIF(data #>> '{originalCostUsd}', '')::double precision,
    estimated_cost_usd,
    0
)
WHERE original_cost_usd = 0
  AND (
      estimated_cost_usd <> 0
      OR NULLIF(data #>> '{externalPoolBilling,rawCostUsd}', '') IS NOT NULL
      OR NULLIF(data #>> '{originalCostUsd}', '') IS NOT NULL
  );

UPDATE usage_rollup_totals
SET total_original_cost_usd = CASE
    WHEN dimension = 'external_pool' AND external_pool_raw_cost_usd <> 0
    THEN external_pool_raw_cost_usd
    ELSE total_estimated_cost_usd
END
WHERE total_original_cost_usd = 0
  AND (total_estimated_cost_usd <> 0 OR external_pool_raw_cost_usd <> 0);

UPDATE usage_rollup_time_buckets
SET total_original_cost_usd = CASE
    WHEN dimension = 'external_pool' AND external_pool_raw_cost_usd <> 0
    THEN external_pool_raw_cost_usd
    ELSE total_estimated_cost_usd
END
WHERE total_original_cost_usd = 0
  AND (total_estimated_cost_usd <> 0 OR external_pool_raw_cost_usd <> 0);

UPDATE usage_rollup_totals
SET external_pool_shaped_cost_usd = external_pool_reported_cost_usd,
    external_pool_uplifted_cost_usd = external_pool_reported_cost_usd,
    external_pool_profit_usd = external_pool_reported_cost_usd - external_pool_raw_cost_usd
WHERE external_pool_requests > 0
  AND external_pool_reported_cost_usd <> 0
  AND external_pool_shaped_cost_usd = 0
  AND external_pool_uplifted_cost_usd = 0
  AND external_pool_profit_usd = 0;

UPDATE usage_rollup_time_buckets
SET external_pool_shaped_cost_usd = external_pool_reported_cost_usd,
    external_pool_uplifted_cost_usd = external_pool_reported_cost_usd,
    external_pool_profit_usd = external_pool_reported_cost_usd - external_pool_raw_cost_usd
WHERE external_pool_requests > 0
  AND external_pool_reported_cost_usd <> 0
  AND external_pool_shaped_cost_usd = 0
  AND external_pool_uplifted_cost_usd = 0
  AND external_pool_profit_usd = 0;

UPDATE usage_credential_cost_summary
SET original_cost_usd = estimated_cost_usd
WHERE original_cost_usd = 0
  AND estimated_cost_usd <> 0;
"#;

const CREDENTIAL_RUNTIME_REVISION_SQL: &str = r#"
ALTER TABLE credential_runtime_state
    ADD COLUMN IF NOT EXISTS revision BIGINT NOT NULL DEFAULT 0;

UPDATE credential_runtime_state
SET revision = 1
WHERE revision = 0;

CREATE TABLE IF NOT EXISTS credential_runtime_mutations (
    operation_id TEXT PRIMARY KEY,
    credential_id BIGINT NOT NULL REFERENCES credential_runtime_state(credential_id) ON DELETE CASCADE,
    mutation_kind TEXT NOT NULL,
    applied_revision BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_credential_runtime_mutations_credential_created
    ON credential_runtime_mutations (credential_id, created_at DESC);
"#;

const CREDENTIAL_RUNTIME_GENERATION_SQL: &str = r#"
ALTER TABLE credential_runtime_state
    ADD COLUMN IF NOT EXISTS generation BIGINT NOT NULL DEFAULT 0;
"#;

const CREDENTIAL_STORAGE_REVISION_SQL: &str = r#"
ALTER TABLE credentials
    ADD COLUMN IF NOT EXISTS revision BIGINT NOT NULL DEFAULT 1;

UPDATE credentials
SET revision = 1
WHERE revision < 1;
"#;

const CREDENTIAL_RUNTIME_MUTATION_CLEANUP_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS idx_credential_runtime_mutations_created_at
    ON credential_runtime_mutations (created_at ASC);
"#;

const CREDENTIAL_STATS_DELTA_BATCH_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS credential_stats_delta_batches (
    operation_id TEXT PRIMARY KEY,
    payload_hash TEXT NOT NULL,
    input_credential_count INTEGER NOT NULL,
    applied_credential_count INTEGER NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_credential_stats_delta_batches_created_at
    ON credential_stats_delta_batches (created_at ASC);
"#;

const USAGE_CLEANUP_BATCH_SIZE_LIMIT_SQL: &str = r#"
ALTER TABLE usage_cleanup_jobs
    DROP CONSTRAINT IF EXISTS usage_cleanup_jobs_batch_size_check;

ALTER TABLE usage_cleanup_jobs
    ADD CONSTRAINT usage_cleanup_jobs_batch_size_check
    CHECK (batch_size > 0 AND batch_size <= 5000);
"#;

const USAGE_ROLLUP_HOUR_BUCKET_COMPRESSION_SQL: &str = r#"
CREATE TEMP TABLE usage_rollup_time_buckets_hourly ON COMMIT DROP AS
SELECT
    date_trunc('hour', bucket_start) AS bucket_start,
    dimension,
    dimension_key,
    NULLIF(MAX(NULLIF(dimension_label, '')), '') AS dimension_label,
    COALESCE(SUM(requests), 0)::bigint AS requests,
    COALESCE(SUM(success_requests), 0)::bigint AS success_requests,
    COALESCE(SUM(error_requests), 0)::bigint AS error_requests,
    COALESCE(SUM(stream_requests), 0)::bigint AS stream_requests,
    COALESCE(SUM(non_stream_requests), 0)::bigint AS non_stream_requests,
    COALESCE(SUM(priced_requests), 0)::bigint AS priced_requests,
    COALESCE(SUM(unpriced_requests), 0)::bigint AS unpriced_requests,
    COALESCE(SUM(local_prompt_cache_requests), 0)::bigint AS local_prompt_cache_requests,
    COALESCE(SUM(simulated_requests), 0)::bigint AS simulated_requests,
    COALESCE(SUM(upstream_metadata_requests), 0)::bigint AS upstream_metadata_requests,
    COALESCE(SUM(sticky_bound_requests), 0)::bigint AS sticky_bound_requests,
    COALESCE(SUM(fallback_from_sticky_requests), 0)::bigint AS fallback_from_sticky_requests,
    COALESCE(SUM(total_input_tokens), 0)::bigint AS total_input_tokens,
    COALESCE(SUM(billable_input_tokens), 0)::bigint AS billable_input_tokens,
    COALESCE(SUM(total_output_tokens), 0)::bigint AS total_output_tokens,
    COALESCE(SUM(total_cache_read_input_tokens), 0)::bigint AS total_cache_read_input_tokens,
    COALESCE(SUM(total_cache_creation_input_tokens), 0)::bigint AS total_cache_creation_input_tokens,
    COALESCE(SUM(local_prompt_cache_input_tokens), 0)::bigint AS local_prompt_cache_input_tokens,
    COALESCE(SUM(local_prompt_cache_read_input_tokens), 0)::bigint AS local_prompt_cache_read_input_tokens,
    COALESCE(SUM(local_prompt_cache_creation_input_tokens), 0)::bigint AS local_prompt_cache_creation_input_tokens,
    COALESCE(SUM(total_estimated_cost_usd), 0)::double precision AS total_estimated_cost_usd,
    COALESCE(SUM(total_original_cost_usd), 0)::double precision AS total_original_cost_usd,
    COALESCE(SUM(total_kiro_metering_usage), 0)::double precision AS total_kiro_metering_usage,
    COALESCE(SUM(external_pool_requests), 0)::bigint AS external_pool_requests,
    COALESCE(SUM(external_pool_priced_requests), 0)::bigint AS external_pool_priced_requests,
    COALESCE(SUM(external_pool_unpriced_requests), 0)::bigint AS external_pool_unpriced_requests,
    COALESCE(SUM(external_pool_cost_floor_applied_requests), 0)::bigint AS external_pool_cost_floor_applied_requests,
    COALESCE(SUM(external_pool_raw_cost_usd), 0)::double precision AS external_pool_raw_cost_usd,
    COALESCE(SUM(external_pool_shaped_cost_usd), 0)::double precision AS external_pool_shaped_cost_usd,
    COALESCE(SUM(external_pool_uplifted_cost_usd), 0)::double precision AS external_pool_uplifted_cost_usd,
    COALESCE(SUM(external_pool_profit_usd), 0)::double precision AS external_pool_profit_usd,
    COALESCE(SUM(external_pool_reported_cost_usd), 0)::double precision AS external_pool_reported_cost_usd,
    COALESCE(SUM(external_pool_billable_cost_usd), 0)::double precision AS external_pool_billable_cost_usd,
    COALESCE(SUM(external_pool_cost_floor_delta_usd), 0)::double precision AS external_pool_cost_floor_delta_usd,
    COALESCE(SUM(duration_ms_sum), 0)::bigint AS duration_ms_sum,
    COALESCE(SUM(duration_ms_count), 0)::bigint AS duration_ms_count,
    COALESCE(MAX(duration_ms_max), 0)::bigint AS duration_ms_max,
    COALESCE(MAX(updated_at), now()) AS updated_at
FROM usage_rollup_time_buckets
GROUP BY date_trunc('hour', bucket_start), dimension, dimension_key;

TRUNCATE TABLE usage_rollup_time_buckets;

INSERT INTO usage_rollup_time_buckets (
    bucket_start, dimension, dimension_key, dimension_label, requests,
    success_requests, error_requests, stream_requests, non_stream_requests,
    priced_requests, unpriced_requests, local_prompt_cache_requests,
    simulated_requests, upstream_metadata_requests, sticky_bound_requests,
    fallback_from_sticky_requests, total_input_tokens, billable_input_tokens,
    total_output_tokens, total_cache_read_input_tokens,
    total_cache_creation_input_tokens, local_prompt_cache_input_tokens,
    local_prompt_cache_read_input_tokens, local_prompt_cache_creation_input_tokens,
    total_estimated_cost_usd, total_original_cost_usd, total_kiro_metering_usage,
    external_pool_requests, external_pool_priced_requests, external_pool_unpriced_requests,
    external_pool_cost_floor_applied_requests, external_pool_raw_cost_usd,
    external_pool_shaped_cost_usd, external_pool_uplifted_cost_usd,
    external_pool_profit_usd, external_pool_reported_cost_usd,
    external_pool_billable_cost_usd, external_pool_cost_floor_delta_usd,
    duration_ms_sum, duration_ms_count, duration_ms_max, updated_at
)
SELECT
    bucket_start, dimension, dimension_key, dimension_label, requests,
    success_requests, error_requests, stream_requests, non_stream_requests,
    priced_requests, unpriced_requests, local_prompt_cache_requests,
    simulated_requests, upstream_metadata_requests, sticky_bound_requests,
    fallback_from_sticky_requests, total_input_tokens, billable_input_tokens,
    total_output_tokens, total_cache_read_input_tokens,
    total_cache_creation_input_tokens, local_prompt_cache_input_tokens,
    local_prompt_cache_read_input_tokens, local_prompt_cache_creation_input_tokens,
    total_estimated_cost_usd, total_original_cost_usd, total_kiro_metering_usage,
    external_pool_requests, external_pool_priced_requests, external_pool_unpriced_requests,
    external_pool_cost_floor_applied_requests, external_pool_raw_cost_usd,
    external_pool_shaped_cost_usd, external_pool_uplifted_cost_usd,
    external_pool_profit_usd, external_pool_reported_cost_usd,
    external_pool_billable_cost_usd, external_pool_cost_floor_delta_usd,
    duration_ms_sum, duration_ms_count, duration_ms_max, updated_at
FROM usage_rollup_time_buckets_hourly;

CREATE TEMP TABLE usage_cache_read_rollup_time_buckets_hourly ON COMMIT DROP AS
SELECT
    date_trunc('hour', bucket_start) AS bucket_start,
    cache_read_input_tokens,
    COALESCE(SUM(requests), 0)::bigint AS requests,
    COALESCE(MAX(updated_at), now()) AS updated_at
FROM usage_cache_read_rollup_time_buckets
GROUP BY date_trunc('hour', bucket_start), cache_read_input_tokens;

TRUNCATE TABLE usage_cache_read_rollup_time_buckets;

INSERT INTO usage_cache_read_rollup_time_buckets (
    bucket_start, cache_read_input_tokens, requests, updated_at
)
SELECT bucket_start, cache_read_input_tokens, requests, updated_at
FROM usage_cache_read_rollup_time_buckets_hourly;

CREATE TEMP TABLE usage_duration_rollup_time_buckets_hourly ON COMMIT DROP AS
SELECT
    date_trunc('hour', bucket_start) AS bucket_start,
    duration_ms,
    COALESCE(SUM(requests), 0)::bigint AS requests,
    COALESCE(MAX(updated_at), now()) AS updated_at
FROM usage_duration_rollup_time_buckets
GROUP BY date_trunc('hour', bucket_start), duration_ms;

TRUNCATE TABLE usage_duration_rollup_time_buckets;

INSERT INTO usage_duration_rollup_time_buckets (
    bucket_start, duration_ms, requests, updated_at
)
SELECT bucket_start, duration_ms, requests, updated_at
FROM usage_duration_rollup_time_buckets_hourly;
"#;

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    use super::*;
    use crate::anthropic::pricing::{ModelPriceItem, ModelPricing};
    use crate::anthropic::usage::{
        ExternalPoolBilling, ExternalPoolUsageSnapshot, UsageRecordStatus, UsageRouteKind,
        UsageRouteSubtype, UsageSource,
    };
    use crate::model::config::{ReportedUsageFieldPolicy, ReportedUsagePathPolicy};

    fn test_config() -> Option<Config> {
        let url = crate::storage::integration_test_url("KIRO_RS_TEST_POSTGRES_URL")?;
        let mut config = Config::default();
        config.postgres.url = Some(url);
        config.postgres.max_connections = 2;
        Some(config)
    }

    #[test]
    fn required_postgres_schema_columns_cover_known_upgrade_breakers() {
        let required: HashSet<(&str, &str)> = REQUIRED_POSTGRES_SCHEMA_COLUMNS
            .iter()
            .map(|column| (column.table_name, column.column_name))
            .collect();
        for pair in [
            ("external_upstream_pools", "revision"),
            ("external_upstream_pools", "pre_output_stream_retry_mode"),
            ("external_upstream_pools", "route_mode"),
            ("external_upstream_pools", "route_rules"),
            ("usage_records", "rollup_active"),
            ("usage_cleanup_jobs", "batch_size"),
            ("usage_records", "original_cost_usd"),
            ("usage_records", "kiro_metering_usage"),
            ("model_capabilities_sync_status", "reasoning_fields"),
            ("credentials", "revision"),
            ("credential_runtime_state", "generation"),
            ("credential_runtime_mutations", "operation_id"),
            ("credential_stats_delta_batches", "operation_id"),
        ] {
            assert!(
                required.contains(&pair),
                "missing required schema guard for {pair:?}"
            );
        }
    }

    #[test]
    fn required_postgres_schema_missing_columns_reports_table_and_column() {
        let present: HashSet<(String, String)> = REQUIRED_POSTGRES_SCHEMA_COLUMNS
            .iter()
            .filter(|required| {
                !(required.table_name == "external_upstream_pools"
                    && required.column_name == "revision")
            })
            .map(|required| {
                (
                    required.table_name.to_string(),
                    required.column_name.to_string(),
                )
            })
            .collect();

        let missing = required_postgres_schema_missing_columns(&present);
        assert_eq!(missing, vec!["external_upstream_pools.revision"]);
    }

    async fn clean(store: &PostgresStore) {
        for statement in [
            "TRUNCATE TABLE admin_audit_logs",
            "TRUNCATE TABLE credential_events",
            "TRUNCATE TABLE usage_cleanup_jobs",
            "TRUNCATE TABLE usage_cleanup_watermarks",
            "TRUNCATE TABLE usage_credential_cost_summary",
            "TRUNCATE TABLE usage_duration_rollup_time_buckets",
            "TRUNCATE TABLE usage_cache_read_rollup_time_buckets",
            "TRUNCATE TABLE usage_cache_read_totals",
            "TRUNCATE TABLE usage_rollup_time_buckets",
            "TRUNCATE TABLE usage_rollup_totals",
            "TRUNCATE TABLE usage_records",
            "TRUNCATE TABLE model_pricing_sync_status",
            "TRUNCATE TABLE model_pricing",
            "TRUNCATE TABLE credential_stats_delta_batches",
            "TRUNCATE TABLE credential_runtime_mutations",
            "TRUNCATE TABLE credential_runtime_state CASCADE",
            "TRUNCATE TABLE credential_stats CASCADE",
            "TRUNCATE TABLE credentials CASCADE",
            "TRUNCATE TABLE proxy_resources CASCADE",
            "TRUNCATE TABLE external_upstream_pools CASCADE",
            "TRUNCATE TABLE runtime_config",
        ] {
            sqlx::query(statement).execute(store.pool()).await.unwrap();
        }
    }

    #[tokio::test]
    async fn postgres_schema_compatibility_check_rejects_missing_upgrade_column() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let store = PostgresStore::connect_test(&config).await.unwrap();

        sqlx::query("ALTER TABLE external_upstream_pools DROP COLUMN revision")
            .execute(store.pool())
            .await
            .unwrap();

        let error = store
            .verify_required_schema_compatibility(false)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("external_upstream_pools.revision"));
        assert!(error.contains("KIRO_RS_POSTGRES_MIGRATE_ON_START=true"));
        assert!(error.contains("startup migration is disabled"));

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_usage_pool_isolated_from_exhausted_main_pool_for_three_rounds() {
        let Some(mut config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        config.postgres.max_connections = 1;
        config.postgres.usage_max_connections = 1;

        let main_store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&main_store).await;
        let usage_store = Arc::new(
            PostgresStore::connect_usage_test_peer(&config, main_store.as_ref())
                .await
                .unwrap(),
        );
        let usage = PostgresUsageStore::new(usage_store);

        for round in 0..3 {
            let holder = main_store.pool().acquire().await.unwrap();
            let mut record = usage_record(&format!("isolated-usage-pool-{round}"), round);
            record.conversation_id = Some(format!("usage-pool-isolation-{round}"));
            let write = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                usage.record_batch(vec![record]),
            )
            .await;
            drop(holder);

            let result = write.unwrap_or_else(|_| {
                panic!("round {round}: usage write waited on the exhausted main PgSQL pool")
            });
            assert!(
                result.is_ok(),
                "round {round}: isolated usage PgSQL pool write failed: {result:?}"
            );
        }

        main_store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_startup_migration_repairs_usage_dashboard_upgrade_columns() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;

        for statement in [
            "ALTER TABLE usage_records DROP COLUMN pricing_available",
            "ALTER TABLE usage_records DROP COLUMN duration_ms",
            "ALTER TABLE usage_records DROP COLUMN data",
            "ALTER TABLE usage_rollup_totals DROP COLUMN sticky_bound_requests",
            "ALTER TABLE usage_rollup_totals DROP COLUMN external_pool_reported_cost_usd",
            "ALTER TABLE usage_rollup_totals DROP COLUMN duration_ms_sum",
            "ALTER TABLE usage_rollup_time_buckets DROP COLUMN sticky_bound_requests",
            "ALTER TABLE usage_rollup_time_buckets DROP COLUMN external_pool_reported_cost_usd",
            "ALTER TABLE usage_rollup_time_buckets DROP COLUMN duration_ms_sum",
            "ALTER TABLE usage_credential_cost_summary DROP COLUMN priced_requests",
        ] {
            sqlx::query(statement).execute(store.pool()).await.unwrap();
        }

        let missing_before = store
            .verify_required_schema_compatibility(false)
            .await
            .unwrap_err()
            .to_string();
        assert!(missing_before.contains("usage_records.pricing_available"));
        assert!(missing_before.contains("usage_rollup_totals.external_pool_reported_cost_usd"));
        assert!(missing_before.contains("usage_rollup_time_buckets.duration_ms_sum"));

        store.migrate_with_options(false).await.unwrap();
        store
            .verify_required_schema_compatibility(true)
            .await
            .unwrap();

        let usage = PostgresUsageStore::new(Arc::new(store.clone()));
        usage
            .record_batch(vec![usage_record("dashboard-upgrade-repaired", 750)])
            .await
            .unwrap();
        let dashboard = usage.dashboard(Some("UTC"), 500).await.unwrap();
        assert!(
            dashboard
                .windows
                .iter()
                .any(|window| window.summary.total_requests >= 1),
            "dashboard should load after usage dashboard upgrade-column repair"
        );

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_startup_migration_expands_usage_cleanup_batch_size_constraint() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;

        sqlx::query(
            "DELETE FROM schema_migrations WHERE version = 'usage-cleanup-batch-size-limit-v1'",
        )
        .execute(store.pool())
        .await
        .unwrap();
        sqlx::query(
            r#"
            ALTER TABLE usage_cleanup_jobs
                DROP CONSTRAINT IF EXISTS usage_cleanup_jobs_batch_size_check
            "#,
        )
        .execute(store.pool())
        .await
        .unwrap();
        sqlx::query(
            r#"
            ALTER TABLE usage_cleanup_jobs
                ADD CONSTRAINT usage_cleanup_jobs_batch_size_check
                CHECK (batch_size > 0 AND batch_size <= 500)
            "#,
        )
        .execute(store.pool())
        .await
        .unwrap();

        let old_constraint_error = sqlx::query(
            r#"
            INSERT INTO usage_cleanup_jobs (
                job_id, mode, cutoff_at, batch_size, max_batches,
                pause_ms_between_batches, status
            )
            VALUES ($1, 'soft_delete', now(), 501, 100, 0, 'queued')
            "#,
        )
        .bind("cleanup-old-limit-rejected")
        .execute(store.pool())
        .await
        .unwrap_err()
        .to_string();
        assert!(old_constraint_error.contains("usage_cleanup_jobs_batch_size_check"));

        let compatibility_error = store
            .verify_required_schema_compatibility(false)
            .await
            .unwrap_err()
            .to_string();
        assert!(compatibility_error.contains("usage_cleanup_jobs.batch_size_check<=5000"));

        store.migrate_with_options(false).await.unwrap();
        store
            .verify_required_schema_compatibility(true)
            .await
            .unwrap();
        let usage_store = PostgresUsageStore::new(Arc::new(store.clone()));
        assert!(
            usage_store
                .create_cleanup_job(NewUsageCleanupJob {
                    job_id: "cleanup-new-limit-accepted",
                    mode: "soft_delete",
                    cutoff_at: Utc::now(),
                    batch_size: 5_000,
                    max_batches: 100,
                    pause_ms_between_batches: 0,
                })
                .await
                .unwrap(),
            "expanded cleanup batch-size constraint must accept 5000"
        );

        store.drop_test_schema().await.unwrap();
    }

    fn usage_record(id: &str, cache_read: i32) -> UsageRecord {
        UsageRecord {
            id: id.to_string(),
            created_at: Utc::now().to_rfc3339(),
            endpoint: "/v1/messages".to_string(),
            stream: false,
            model: "claude-sonnet-4-5".to_string(),
            requested_max_tokens: None,
            downstream_stop_reason: None,
            upstream_model: None,
            external_outbound_model: None,
            model_resolution_source: None,
            model_resolution_note: None,
            conversation_id: Some("session-a".to_string()),
            request_api_key_id: Some("request-key-a".to_string()),
            credential_id: Some(7),
            credential_label: Some("alpha@example.com".to_string()),
            status: UsageRecordStatus::Success,
            usage_source: UsageSource::LocalPromptCache,
            raw_usage: None,
            total_input_tokens: 100,
            compat_input_tokens: 10,
            billable_input_tokens: 10,
            output_tokens: 20,
            cache_read_input_tokens: cache_read,
            cache_creation_input_tokens: 5,
            cache_creation_5m_input_tokens: 5,
            cache_creation_1h_input_tokens: 0,
            estimated_cost_usd: 0.001,
            original_cost_usd: 0.001,
            kiro_metering_usage: 0.0,
            pricing_available: true,
            pricing_model: Some("claude-sonnet-4-5".to_string()),
            duration_ms: 30,
            first_token_latency_ms: None,
            response_latency_ms: Some(30),
            latency_trace: None,
            simulated: true,
            sticky_bound: true,
            fallback_from_sticky: false,
            route_kind: None,
            route_subtype: None,
            fallback_reason: None,
            direct_policy_reason: None,
            local_attempted: None,
            local_preflight: None,
            external_pool_id: None,
            external_pool_name: None,
            external_attempts: Vec::new(),
            usage_projection_applied: None,
            external_pool_billing: None,
            credential_attempts: Vec::new(),
            error_type: None,
            error_message: None,
            error_detail: None,
            error_status_code: None,
            error_source: None,
            error_id: None,
            error_metadata: None,
            raw_upstream_error: None,
            public_error_status_code: None,
            public_error_type: None,
            public_error_message: None,
            payload_breakdown: None,
            payload_guard_report: None,
        }
    }

    fn fixed_utc(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .expect("fixed test timestamp")
            .with_timezone(&Utc)
    }

    #[test]
    fn dashboard_window_basic_fallback_preserves_core_series_metrics() {
        let window = usage_dashboard_window_from_series_point(UsageSeriesPoint {
            key: "today".to_string(),
            label: "今日".to_string(),
            from: "2026-07-26T00:00:00Z".to_string(),
            to: "2026-07-26T01:00:00Z".to_string(),
            requests: 100,
            success_requests: 97,
            error_requests: 3,
            total_input_tokens: 12_345,
            billable_input_tokens: 2_345,
            total_output_tokens: 678,
            total_estimated_cost_usd: 1.25,
            total_original_cost_usd: 2.5,
            total_kiro_metering_usage: 3.75,
        });

        assert_eq!(window.key, "today");
        assert_eq!(window.summary.total_requests, 100);
        assert_eq!(window.summary.success_requests, 97);
        assert_eq!(window.summary.error_requests, 3);
        assert_eq!(window.summary.total_kiro_metering_usage, 3.75);
        assert!((window.summary.error_rate - 0.03).abs() < f64::EPSILON);
        assert_eq!(window.summary.total_input_tokens, 12_345);
        assert_eq!(window.summary.billable_input_tokens, 2_345);
        assert_eq!(window.summary.total_output_tokens, 678);
        assert_eq!(window.summary.p95_duration_ms, 0);
        assert_eq!(window.summary.high_cache_requests, 0);
    }

    fn postgres_timestamp_precision(value: DateTime<Utc>) -> DateTime<Utc> {
        let nanos = value.timestamp_subsec_nanos();
        value
            .with_nanosecond((nanos / 1_000) * 1_000)
            .expect("valid PostgreSQL microsecond-precision timestamp")
    }

    fn dashboard_test_window(
        key: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> UsageDashboardWindowSpec {
        UsageDashboardWindowSpec {
            key: key.to_string(),
            label: key.to_string(),
            from,
            to,
        }
    }

    #[tokio::test]
    async fn postgres_usage_cleanup_job_is_persistent_exclusive_and_recoverable() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;
        let usage_store = PostgresUsageStore::new(store.clone());
        let cutoff = Utc::now() - chrono::Duration::days(7);

        assert!(
            usage_store
                .create_cleanup_job(NewUsageCleanupJob {
                    job_id: "cleanup-job-a",
                    mode: "soft_delete",
                    cutoff_at: cutoff,
                    batch_size: 250,
                    max_batches: 100,
                    pause_ms_between_batches: 10,
                })
                .await
                .unwrap()
        );
        assert!(
            !usage_store
                .create_cleanup_job(NewUsageCleanupJob {
                    job_id: "cleanup-job-b",
                    mode: "soft_delete",
                    cutoff_at: cutoff,
                    batch_size: 250,
                    max_batches: 100,
                    pause_ms_between_batches: 10,
                })
                .await
                .unwrap(),
            "partial unique index must reject a second active job"
        );

        let claimed = usage_store
            .claim_cleanup_job("cleanup-job-a", "worker-a", 30)
            .await
            .unwrap()
            .expect("first worker claims queued job");
        assert_eq!(claimed.status, "running");
        assert!(
            usage_store
                .claim_cleanup_job("cleanup-job-a", "worker-b", 30)
                .await
                .unwrap()
                .is_none(),
            "an unexpired lease cannot be stolen"
        );

        let cancelled = usage_store
            .request_cleanup_cancel("cleanup-job-a")
            .await
            .unwrap()
            .expect("running job can be cancelled");
        assert!(cancelled.cancel_requested);
        assert_eq!(cancelled.status, "running");
        let cancel_seen = usage_store
            .update_cleanup_job_progress(
                UsageCleanupJobProgress {
                    job_id: "cleanup-job-a",
                    worker_id: "worker-a",
                    status: "cancelled",
                    phase: "postgres",
                    processed_rows: 17,
                    last_batch_rows: 0,
                    batches: 1,
                    remaining_rows: None,
                    stop_reason: Some("cancel_requested"),
                    last_error: None,
                    redis_deleted_keys: 0,
                    redis_delete_commands: 0,
                    redis_max_command_keys: 0,
                    redis_scan_passes: 8,
                    redis_used_del_fallback: false,
                    redis_pass_limit_reached: true,
                    finished: true,
                },
                30,
            )
            .await
            .unwrap();
        assert_eq!(cancel_seen, Some(true));

        let requeued = usage_store
            .requeue_cleanup_job("cleanup-job-a")
            .await
            .unwrap()
            .expect("cancelled job can be requeued idempotently");
        assert_eq!(requeued.status, "queued");
        assert_eq!(requeued.processed_rows, 17);
        assert_eq!(requeued.redis_scan_passes, 8, "scan passes are cumulative");
        assert!(
            !requeued.redis_pass_limit_reached,
            "a retry must get a fresh convergence result"
        );
        usage_store
            .claim_cleanup_job("cleanup-job-a", "worker-b", 30)
            .await
            .unwrap()
            .expect("requeued job can be claimed");
        sqlx::query(
            "UPDATE usage_cleanup_jobs SET lease_until = now() - interval '1 second' WHERE job_id = $1",
        )
        .bind("cleanup-job-a")
        .execute(store.pool())
        .await
        .unwrap();
        let recovered = usage_store
            .claim_cleanup_job("cleanup-job-a", "worker-c", 30)
            .await
            .unwrap()
            .expect("expired worker lease can be recovered");
        assert_eq!(recovered.processed_rows, 17);

        usage_store
            .update_cleanup_job_progress(
                UsageCleanupJobProgress {
                    job_id: "cleanup-job-a",
                    worker_id: "worker-c",
                    status: "completed",
                    phase: "complete",
                    processed_rows: 17,
                    last_batch_rows: 0,
                    batches: 1,
                    remaining_rows: Some(0),
                    stop_reason: Some("no_more_rows"),
                    last_error: None,
                    redis_deleted_keys: 321,
                    redis_delete_commands: 6,
                    redis_max_command_keys: 64,
                    redis_scan_passes: 2,
                    redis_used_del_fallback: false,
                    redis_pass_limit_reached: false,
                    finished: true,
                },
                30,
            )
            .await
            .unwrap();
        let completed = usage_store
            .cleanup_job("cleanup-job-a")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completed.status, "completed");
        assert_eq!(completed.matched_rows, Some(17));
        assert_eq!(completed.remaining_rows, Some(0));
        assert_eq!(completed.redis_max_command_keys, 64);
        assert!(completed.finished_at.is_some());
        assert!(completed.lease_until.is_none());

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_usage_cleanup_supervisor_only_sees_claimable_jobs_for_three_rounds() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;
        let usage_store = PostgresUsageStore::new(store.clone());

        for round in 0..3 {
            sqlx::query("TRUNCATE TABLE usage_cleanup_jobs")
                .execute(store.pool())
                .await
                .unwrap();
            let job_id = format!("cleanup-supervisor-round-{round}");
            let worker_a = format!("worker-a-{round}");
            let worker_b = format!("worker-b-{round}");
            usage_store
                .create_cleanup_job(NewUsageCleanupJob {
                    job_id: &job_id,
                    mode: "soft_delete",
                    cutoff_at: Utc::now() - chrono::Duration::days(7),
                    batch_size: 250,
                    max_batches: 100,
                    pause_ms_between_batches: 10,
                })
                .await
                .unwrap();
            usage_store
                .claim_cleanup_job(&job_id, &worker_a, 30)
                .await
                .unwrap()
                .expect("first worker claims queued job");

            assert!(
                usage_store
                    .recoverable_cleanup_job()
                    .await
                    .unwrap()
                    .is_none(),
                "round {round}: an unexpired running lease is not supervisor-recoverable"
            );

            sqlx::query(
                "UPDATE usage_cleanup_jobs SET lease_until = now() - interval '1 second' WHERE job_id = $1",
            )
            .bind(&job_id)
            .execute(store.pool())
            .await
            .unwrap();
            let recoverable = usage_store
                .recoverable_cleanup_job()
                .await
                .unwrap()
                .expect("expired running job becomes supervisor-recoverable");
            assert_eq!(recoverable.job_id, job_id);
            let reclaimed = usage_store
                .claim_cleanup_job(&job_id, &worker_b, 30)
                .await
                .unwrap()
                .expect("second worker reclaims expired lease");
            assert_eq!(reclaimed.status, "running");
        }

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_usage_cleanup_cancel_resume_and_expired_recovery_for_three_rounds() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;
        let usage_store = PostgresUsageStore::new(store.clone());

        for round in 0..3 {
            sqlx::query("TRUNCATE TABLE usage_cleanup_jobs")
                .execute(store.pool())
                .await
                .unwrap();
            let job_id = format!("cleanup-cancel-recovery-round-{round}");
            let first_worker = format!("cancel-worker-{round}");
            let expired_worker = format!("expired-worker-{round}");
            let recovered_worker = format!("recovered-worker-{round}");
            usage_store
                .create_cleanup_job(NewUsageCleanupJob {
                    job_id: &job_id,
                    mode: "soft_delete",
                    cutoff_at: Utc::now(),
                    batch_size: 10,
                    max_batches: 10,
                    pause_ms_between_batches: 0,
                })
                .await
                .unwrap();
            usage_store
                .claim_cleanup_job(&job_id, &first_worker, 30)
                .await
                .unwrap()
                .expect("queued job is claimed");
            assert!(
                usage_store
                    .request_cleanup_cancel(&job_id)
                    .await
                    .unwrap()
                    .expect("running job accepts cancellation")
                    .cancel_requested
            );
            usage_store
                .update_cleanup_job_progress(
                    UsageCleanupJobProgress {
                        job_id: &job_id,
                        worker_id: &first_worker,
                        status: "cancelled",
                        phase: "postgres",
                        processed_rows: round as u64,
                        last_batch_rows: 0,
                        batches: round,
                        remaining_rows: None,
                        stop_reason: Some("cancel_requested"),
                        last_error: None,
                        redis_deleted_keys: 0,
                        redis_delete_commands: 0,
                        redis_max_command_keys: 0,
                        redis_scan_passes: 0,
                        redis_used_del_fallback: false,
                        redis_pass_limit_reached: false,
                        finished: true,
                    },
                    30,
                )
                .await
                .unwrap()
                .expect("lease owner persists cancellation");
            usage_store
                .requeue_cleanup_job(&job_id)
                .await
                .unwrap()
                .expect("cancelled job resumes");
            usage_store
                .claim_cleanup_job(&job_id, &expired_worker, 30)
                .await
                .unwrap()
                .expect("resumed job is claimed");
            sqlx::query(
                "UPDATE usage_cleanup_jobs SET lease_until = now() - interval '1 second' WHERE job_id = $1",
            )
            .bind(&job_id)
            .execute(store.pool())
            .await
            .unwrap();
            let recovered = usage_store
                .claim_cleanup_job(&job_id, &recovered_worker, 30)
                .await
                .unwrap()
                .expect("expired resumed job is recovered");
            assert_eq!(recovered.processed_rows, round as u64, "round {round}");
            assert!(!recovered.cancel_requested, "round {round}");
        }

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_usage_cleanup_batches_are_bounded_idempotent_and_skip_locked() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;
        let usage_store = PostgresUsageStore::new(store.clone());
        let old = Utc::now() - chrono::Duration::days(30);
        let mut records = Vec::new();
        for index in 0..5 {
            let mut record = usage_record(&format!("cleanup-row-{index}"), index);
            record.created_at = (old + chrono::Duration::seconds(index as i64)).to_rfc3339();
            records.push(record);
        }
        usage_store.record_batch(records).await.unwrap();

        let mut blocker = store.pool().begin().await.unwrap();
        sqlx::query("SELECT id FROM usage_records WHERE id = $1 FOR UPDATE")
            .bind("cleanup-row-0")
            .fetch_one(&mut *blocker)
            .await
            .unwrap();
        assert_eq!(
            usage_store
                .soft_delete_cleanup_batch(Utc::now(), 2)
                .await
                .unwrap()
                .processed_rows,
            2,
            "the batch must skip a locked oldest row instead of waiting on it"
        );
        blocker.rollback().await.unwrap();

        for expected in [2, 1, 0] {
            assert_eq!(
                usage_store
                    .soft_delete_cleanup_batch(Utc::now(), 2)
                    .await
                    .unwrap()
                    .processed_rows,
                expected
            );
        }
        assert_eq!(
            usage_store
                .preview_soft_delete_cleanup(Utc::now())
                .await
                .unwrap()
                .matched_rows,
            0
        );

        let hard_cutoff = Utc::now() + chrono::Duration::seconds(1);
        for expected in [2, 2, 1, 0] {
            assert_eq!(
                usage_store
                    .hard_delete_cleanup_batch(hard_cutoff, 2)
                    .await
                    .unwrap()
                    .processed_rows,
                expected
            );
        }

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_usage_cleanup_locked_rows_remain_visible_until_release_for_three_rounds() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;
        let usage_store = PostgresUsageStore::new(store.clone());

        for round in 0..3 {
            let row_id = format!("cleanup-locked-only-round-{round}");
            let mut record = usage_record(&row_id, round);
            record.created_at = Utc::now().to_rfc3339();
            usage_store.record(record).await.unwrap();

            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            let soft_cutoff = Utc::now();
            let mut soft_blocker = store.pool().begin().await.unwrap();
            sqlx::query("SELECT id FROM usage_records WHERE id = $1 FOR UPDATE")
                .bind(&row_id)
                .fetch_one(&mut *soft_blocker)
                .await
                .unwrap();
            for attempt in 0..3 {
                let result = usage_store
                    .soft_delete_cleanup_batch(soft_cutoff, 2)
                    .await
                    .unwrap();
                assert_eq!(result.processed_rows, 0, "round {round}, attempt {attempt}");
                assert_eq!(
                    result.has_remaining,
                    Some(true),
                    "round {round}, attempt {attempt}: a locked matching row must not look complete"
                );
            }
            soft_blocker.rollback().await.unwrap();

            let soft_recovered = usage_store
                .soft_delete_cleanup_batch(soft_cutoff, 2)
                .await
                .unwrap();
            assert_eq!(soft_recovered.processed_rows, 1, "round {round}");
            assert_eq!(soft_recovered.has_remaining, Some(false), "round {round}");

            let hard_cutoff = Utc::now() + chrono::Duration::seconds(1);
            let mut hard_blocker = store.pool().begin().await.unwrap();
            sqlx::query("SELECT id FROM usage_records WHERE id = $1 FOR UPDATE")
                .bind(&row_id)
                .fetch_one(&mut *hard_blocker)
                .await
                .unwrap();
            for attempt in 0..3 {
                let result = usage_store
                    .hard_delete_cleanup_batch(hard_cutoff, 2)
                    .await
                    .unwrap();
                assert_eq!(result.processed_rows, 0, "round {round}, attempt {attempt}");
                assert_eq!(
                    result.has_remaining,
                    Some(true),
                    "round {round}, attempt {attempt}: a locked tombstone must not look complete"
                );
            }
            hard_blocker.rollback().await.unwrap();

            let hard_recovered = usage_store
                .hard_delete_cleanup_batch(hard_cutoff, 2)
                .await
                .unwrap();
            assert_eq!(hard_recovered.processed_rows, 1, "round {round}");
            assert_eq!(hard_recovered.has_remaining, Some(false), "round {round}");
        }

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_usage_cleanup_advisory_guards_are_test_schema_scoped() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let store_a = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        let store_b = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store_a).await;
        clean(&store_b).await;
        let usage_b = PostgresUsageStore::new(store_b.clone());

        let mut record = usage_record("schema-scoped-cleanup-row", 123);
        record.created_at = Utc::now().to_rfc3339();
        usage_b.record(record).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        let cutoff = Utc::now();

        let mut writer_in_other_schema = store_a.pool().begin().await.unwrap();
        acquire_usage_writer_commit_guard(&mut writer_in_other_schema)
            .await
            .unwrap();

        let cleaned = usage_b.soft_delete_cleanup_batch(cutoff, 10).await.unwrap();
        assert_eq!(
            cleaned.processed_rows, 1,
            "a writer guard in another test schema must not trigger cleanup contention"
        );
        assert_eq!(cleaned.has_remaining, Some(false));

        writer_in_other_schema.rollback().await.unwrap();
        store_b.drop_test_schema().await.unwrap();
        store_a.drop_test_schema().await.unwrap();
    }

    async fn assert_postgres_usage_cleanup_authorities_empty(
        store: &PostgresStore,
        usage_store: &PostgresUsageStore,
        context: &str,
    ) {
        let active_rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM usage_records WHERE deleted_at IS NULL",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(active_rows, 0, "{context}: active detail rows");

        let summary = usage_store.summary(500).await.unwrap();
        assert_eq!(summary.total_requests, 0, "{context}: summary requests");
        assert_eq!(
            summary.total_input_tokens, 0,
            "{context}: summary input tokens"
        );
        assert!(
            summary.total_estimated_cost_usd.abs() < 1e-12,
            "{context}: summary cost was {}",
            summary.total_estimated_cost_usd
        );
        assert!(
            usage_store
                .credential_cost_summary()
                .await
                .unwrap()
                .is_empty(),
            "{context}: credential cost summary"
        );

        let dashboard = usage_store.dashboard(Some("UTC"), 500).await.unwrap();
        for window in dashboard.windows {
            assert_eq!(
                window.summary.total_requests, 0,
                "{context}: dashboard {} requests",
                window.key
            );
            assert!(
                window.summary.total_estimated_cost_usd.abs() < 1e-12,
                "{context}: dashboard {} cost was {}",
                window.key,
                window.summary.total_estimated_cost_usd
            );
        }
    }

    async fn zero_stored_usage_json_original_cost(store: &PostgresStore, id: &str) {
        let updated = sqlx::query(
            r#"
            UPDATE usage_records
            SET data = jsonb_set(
                    data,
                    '{originalCostUsd}',
                    to_jsonb(0.0::double precision)
                ),
                updated_at = now()
            WHERE id = $1
            "#,
        )
        .bind(id)
        .execute(store.pool())
        .await
        .unwrap();
        assert_eq!(updated.rows_affected(), 1, "historical fixture row exists");
    }

    #[tokio::test]
    async fn postgres_cleanup_rollup_update_subtracts_legacy_cost_for_three_rounds() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        let usage_store = PostgresUsageStore::new(store.clone());

        for round in 0..3 {
            clean(&store).await;
            let local_id = format!("legacy-local-update-round-{round}");
            let mut old_local = usage_record(&local_id, 800);
            old_local.estimated_cost_usd = 0.8;
            old_local.original_cost_usd = 0.8;
            old_local.duration_ms = 900;
            usage_store.record(old_local.clone()).await.unwrap();
            zero_stored_usage_json_original_cost(&store, &local_id).await;

            let mut new_local = usage_record(&local_id, 120);
            new_local.created_at = old_local.created_at;
            new_local.estimated_cost_usd = 0.2;
            new_local.original_cost_usd = 0.2;
            new_local.duration_ms = 120;
            usage_store.record(new_local).await.unwrap();

            let local_rollup: (i64, f64, f64, i64) = sqlx::query_as(
                r#"
                SELECT requests, total_estimated_cost_usd,
                       total_original_cost_usd, duration_ms_max
                FROM usage_rollup_totals
                WHERE dimension = 'global' AND dimension_key = 'all'
                "#,
            )
            .fetch_one(store.pool())
            .await
            .unwrap();
            assert_eq!(local_rollup.0, 1, "local round {round}");
            assert!((local_rollup.1 - 0.2).abs() < 1e-12, "local round {round}");
            assert!((local_rollup.2 - 0.2).abs() < 1e-12, "local round {round}");
            assert_eq!(local_rollup.3, 120, "local round {round}");
            let local_credential: (i64, f64, f64) = sqlx::query_as(
                r#"
                SELECT requests, estimated_cost_usd, original_cost_usd
                FROM usage_credential_cost_summary
                WHERE credential_id = 7
                "#,
            )
            .fetch_one(store.pool())
            .await
            .unwrap();
            assert_eq!(local_credential.0, 1, "local round {round}");
            assert!(
                (local_credential.1 - 0.2).abs() < 1e-12,
                "local round {round}"
            );
            assert!(
                (local_credential.2 - 0.2).abs() < 1e-12,
                "local round {round}"
            );

            clean(&store).await;
            let external_id = format!("legacy-external-update-round-{round}");
            let mut old_external = external_usage_record(&external_id, 0.7, 0.8, 0.9);
            old_external.duration_ms = 800;
            usage_store.record(old_external.clone()).await.unwrap();
            zero_stored_usage_json_original_cost(&store, &external_id).await;

            let mut new_external = external_usage_record(&external_id, 0.15, 0.18, 0.2);
            new_external.created_at = old_external.created_at;
            new_external.duration_ms = 80;
            usage_store.record(new_external).await.unwrap();

            let external_rollup: (i64, f64, f64, f64, i64) = sqlx::query_as(
                r#"
                SELECT requests, total_estimated_cost_usd, total_original_cost_usd,
                       external_pool_raw_cost_usd, duration_ms_max
                FROM usage_rollup_totals
                WHERE dimension = 'global' AND dimension_key = 'all'
                "#,
            )
            .fetch_one(store.pool())
            .await
            .unwrap();
            assert_eq!(external_rollup.0, 1, "external round {round}");
            assert!(
                (external_rollup.1 - 0.2).abs() < 1e-12,
                "external round {round}"
            );
            assert!(
                (external_rollup.2 - 0.15).abs() < 1e-12,
                "external round {round}"
            );
            assert!(
                (external_rollup.3 - 0.15).abs() < 1e-12,
                "external round {round}"
            );
            assert_eq!(external_rollup.4, 80, "external round {round}");
        }

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_external_pool_usage_risk_reports_cache_and_cost_risks() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;
        let usage_store = PostgresUsageStore::new(store.clone());

        let mut high_cache = external_usage_record("external-risk-high-cache", 0.20, 0.18, 0.19);
        high_cache.external_pool_billing.as_mut().unwrap().raw_usage =
            external_usage_snapshot(1_000, 20, 910_000, 120_000);
        high_cache.raw_usage = high_cache
            .external_pool_billing
            .as_ref()
            .map(|billing| billing.raw_usage);
        usage_store.record(high_cache).await.unwrap();

        let ok = external_usage_record("external-risk-ok", 0.10, 0.11, 0.12);
        usage_store.record(ok).await.unwrap();

        let response = usage_store
            .external_pool_usage_risk(
                UsageExternalPoolRiskQuery {
                    timezone: "UTC".to_string(),
                    window_key: "custom".to_string(),
                    window_label: "自定义".to_string(),
                    from: Utc::now() - chrono::Duration::minutes(5),
                    to: Utc::now() + chrono::Duration::minutes(5),
                    warning_threshold_tokens: 800_000,
                    critical_threshold_tokens: 1_000_000,
                    pool_id: Some(42),
                    endpoint: None,
                    model: None,
                    stream: None,
                    limit: 10,
                },
                UsageExternalPoolRiskCostConfig {
                    cost_floor_enabled: true,
                    cost_floor_margin_percent: 10,
                },
            )
            .await
            .unwrap();

        assert_eq!(response.totals.records, 2);
        assert_eq!(response.raw_cache.read_warning_count, 1);
        assert_eq!(response.raw_cache.read_critical_count, 0);
        assert_eq!(response.reported_cache.either_warning_count, 0);
        assert_eq!(response.cost.below_raw_count, 1);
        assert_eq!(response.cost.below_target_count, 1);
        assert!(response.cost.total_target_gap_usd > 0.02);
        assert!(
            response
                .buckets
                .iter()
                .any(|bucket| bucket.key == "800k_1m" && bucket.raw_read_count == 1)
        );
        assert_eq!(response.by_pool[0].key, "42");
        assert!(response.samples.iter().any(|sample| {
            sample.id == "external-risk-high-cache"
                && sample
                    .risk_reasons
                    .iter()
                    .any(|reason| reason == "raw_cache_warning")
                && sample
                    .risk_reasons
                    .iter()
                    .any(|reason| reason == "below_target_cost")
        }));

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_soft_cleanup_removes_rollups_and_costs_for_three_rounds() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;
        let usage_store = PostgresUsageStore::new(store.clone());

        for round in 0..3 {
            let mut record = usage_record(&format!("cleanup-soft-rollup-round-{round}"), 600);
            record.created_at = Utc::now().to_rfc3339();
            usage_store.record(record).await.unwrap();
            assert_eq!(usage_store.summary(500).await.unwrap().total_requests, 1);
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            let cutoff = Utc::now();
            let result = usage_store
                .soft_delete_cleanup_batch(cutoff, 10)
                .await
                .unwrap();
            assert_eq!(result.processed_rows, 1, "round {round}");
            assert_eq!(result.has_remaining, Some(false), "round {round}");
            assert_postgres_usage_cleanup_authorities_empty(
                &store,
                &usage_store,
                &format!("soft round {round}"),
            )
            .await;
        }

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_cleanup_recomputes_global_duration_max_for_three_rounds() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;
        let usage_store = PostgresUsageStore::new(store.clone());

        for round in 0..3 {
            let old_created_at = Utc::now();
            let cutoff = old_created_at + chrono::Duration::milliseconds(1);
            let remaining_created_at = old_created_at + chrono::Duration::milliseconds(2);
            let mut removed_max = usage_record(&format!("duration-max-round-{round}"), 10);
            removed_max.created_at = old_created_at.to_rfc3339();
            removed_max.duration_ms = 900;
            let mut remaining = usage_record(&format!("duration-remaining-round-{round}"), 20);
            remaining.created_at = remaining_created_at.to_rfc3339();
            remaining.duration_ms = 120;
            usage_store
                .record_batch(vec![removed_max, remaining])
                .await
                .unwrap();

            let total_before: i64 = sqlx::query_scalar(
                "SELECT duration_ms_max FROM usage_rollup_totals WHERE dimension = 'global' AND dimension_key = 'all'",
            )
            .fetch_one(store.pool())
            .await
            .unwrap();
            let bucket_before: i64 = sqlx::query_scalar(
                r#"
                SELECT duration_ms_max
                FROM usage_rollup_time_buckets
                WHERE bucket_start = date_trunc('hour', $1::timestamptz)
                  AND dimension = 'global'
                  AND dimension_key = 'all'
                "#,
            )
            .bind(old_created_at)
            .fetch_one(store.pool())
            .await
            .unwrap();
            assert_eq!(total_before, 900, "round {round}");
            assert_eq!(bucket_before, 900, "round {round}");

            let result = usage_store
                .soft_delete_cleanup_batch(cutoff, 10)
                .await
                .unwrap();
            assert_eq!(result.processed_rows, 1, "round {round}");
            let total_after: i64 = sqlx::query_scalar(
                "SELECT duration_ms_max FROM usage_rollup_totals WHERE dimension = 'global' AND dimension_key = 'all'",
            )
            .fetch_one(store.pool())
            .await
            .unwrap();
            let bucket_after: i64 = sqlx::query_scalar(
                r#"
                SELECT duration_ms_max
                FROM usage_rollup_time_buckets
                WHERE bucket_start = date_trunc('hour', $1::timestamptz)
                  AND dimension = 'global'
                  AND dimension_key = 'all'
                "#,
            )
            .bind(old_created_at)
            .fetch_one(store.pool())
            .await
            .unwrap();
            assert_eq!(total_after, 120, "round {round}");
            assert_eq!(bucket_after, 120, "round {round}");

            usage_store
                .soft_delete_cleanup_batch(
                    remaining_created_at + chrono::Duration::milliseconds(1),
                    10,
                )
                .await
                .unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_hard_cleanup_removes_legacy_rollups_once_for_three_rounds() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;
        let usage_store = PostgresUsageStore::new(store.clone());

        for round in 0..3 {
            let id = format!("cleanup-hard-legacy-round-{round}");
            usage_store.record(usage_record(&id, 600)).await.unwrap();
            let deleted_at = Utc::now() - chrono::Duration::seconds(1);
            sqlx::query("UPDATE usage_records SET deleted_at = $2, updated_at = $2 WHERE id = $1")
                .bind(&id)
                .bind(deleted_at)
                .execute(store.pool())
                .await
                .unwrap();
            assert_eq!(usage_store.summary(500).await.unwrap().total_requests, 1);

            let result = usage_store
                .hard_delete_cleanup_batch(deleted_at + chrono::Duration::seconds(1), 10)
                .await
                .unwrap();
            assert_eq!(result.processed_rows, 1, "round {round}");
            assert_eq!(result.has_remaining, Some(false), "round {round}");
            assert_postgres_usage_cleanup_authorities_empty(
                &store,
                &usage_store,
                &format!("hard legacy round {round}"),
            )
            .await;
        }

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_cleanup_rejects_late_replay_but_accepts_newer_records_for_three_rounds() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;
        let usage_store = PostgresUsageStore::new(store.clone());

        for round in 0..3 {
            let old_id = format!("cleanup-replay-old-round-{round}");
            let mut old_record = usage_record(&old_id, 600);
            old_record.created_at = Utc::now().to_rfc3339();
            usage_store.record(old_record.clone()).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            let cutoff = Utc::now();
            usage_store
                .soft_delete_cleanup_batch(cutoff, 10)
                .await
                .unwrap();
            assert_postgres_usage_cleanup_authorities_empty(
                &store,
                &usage_store,
                &format!("soft before replay round {round}"),
            )
            .await;

            usage_store.record(old_record.clone()).await.unwrap();
            assert_postgres_usage_cleanup_authorities_empty(
                &store,
                &usage_store,
                &format!("soft replay round {round}"),
            )
            .await;

            usage_store
                .hard_delete_cleanup_batch(Utc::now() + chrono::Duration::seconds(1), 10)
                .await
                .unwrap();
            usage_store.record(old_record).await.unwrap();
            assert_postgres_usage_cleanup_authorities_empty(
                &store,
                &usage_store,
                &format!("hard replay round {round}"),
            )
            .await;

            let new_id = format!("cleanup-replay-new-round-{round}");
            let mut new_record = usage_record(&new_id, 600);
            new_record.created_at = Utc::now().to_rfc3339();
            usage_store.record(new_record).await.unwrap();
            assert_eq!(
                usage_store.summary(500).await.unwrap().total_requests,
                1,
                "round {round}: a record newer than the cleanup cutoff remains valid"
            );

            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            usage_store
                .soft_delete_cleanup_batch(Utc::now(), 10)
                .await
                .unwrap();
            usage_store
                .hard_delete_cleanup_batch(Utc::now() + chrono::Duration::seconds(1), 10)
                .await
                .unwrap();
        }

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_cleanup_is_consistent_with_concurrent_old_writes_for_three_rounds() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;
        let usage_store = PostgresUsageStore::new(store.clone());

        for round in 0..3 {
            let created_at = Utc::now();
            let records = (0..32)
                .map(|index| {
                    let mut record = usage_record(
                        &format!("cleanup-concurrent-round-{round}-record-{index}"),
                        600,
                    );
                    record.created_at = created_at.to_rfc3339();
                    record
                })
                .collect::<Vec<_>>();
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            let cutoff = Utc::now();
            let barrier = Arc::new(tokio::sync::Barrier::new(2));

            let writer_store = usage_store.clone();
            let writer_barrier = barrier.clone();
            let writer = tokio::spawn(async move {
                writer_barrier.wait().await;
                writer_store.record_batch(records).await
            });
            let cleanup_store = usage_store.clone();
            let cleanup_barrier = barrier.clone();
            let cleanup = tokio::spawn(async move {
                cleanup_barrier.wait().await;
                cleanup_store.soft_delete_cleanup_batch(cutoff, 10).await
            });
            writer.await.unwrap().unwrap();
            cleanup.await.unwrap().unwrap();

            loop {
                let result = usage_store
                    .soft_delete_cleanup_batch(cutoff, 10)
                    .await
                    .unwrap();
                if result.has_remaining == Some(false) {
                    break;
                }
            }
            assert_postgres_usage_cleanup_authorities_empty(
                &store,
                &usage_store,
                &format!("concurrent write round {round}"),
            )
            .await;
        }

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn postgres_cleanup_watermark_waits_for_inflight_usage_commit_for_three_rounds() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;
        let usage_store = PostgresUsageStore::new(store.clone());

        for round in 0..3 {
            let mut record = usage_record(
                &format!("cleanup-inflight-commit-round-{round}"),
                700 + round,
            );
            record.created_at = Utc::now().to_rfc3339();
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            let cutoff = Utc::now();

            let mut writer_tx = store.pool().begin().await.unwrap();
            acquire_usage_writer_commit_guard(&mut writer_tx)
                .await
                .unwrap();
            assert!(
                upsert_usage_record_in_tx(&mut writer_tx, &record)
                    .await
                    .unwrap()
            );
            let mut rollups = UsageRollupBatchDelta::default();
            rollups.add_record(&record, 1);
            rollups.apply(&mut writer_tx).await.unwrap();

            let advancing_store = usage_store.clone();
            let mut advancing = tokio::spawn(async move {
                advancing_store
                    .advance_soft_delete_cleanup_watermark(cutoff)
                    .await
            });
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(50), &mut advancing)
                    .await
                    .is_err(),
                "round {round}: cleanup watermark advanced before an older usage transaction committed"
            );

            writer_tx.commit().await.unwrap();
            assert_eq!(
                advancing.await.unwrap().unwrap(),
                postgres_timestamp_precision(cutoff),
                "round {round}"
            );

            let result = usage_store
                .soft_delete_cleanup_batch(cutoff, 10)
                .await
                .unwrap();
            assert_eq!(result.processed_rows, 1, "round {round}");
            assert_eq!(result.has_remaining, Some(false), "round {round}");
            assert_postgres_usage_cleanup_authorities_empty(
                &store,
                &usage_store,
                &format!("inflight commit round {round}"),
            )
            .await;
        }

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_usage_cleanup_batches_return_contention_signal_while_writer_guard_is_held_for_three_rounds()
     {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;
        let usage_store = PostgresUsageStore::new(store.clone());

        for round in 0..3 {
            let row_id = format!("cleanup-writer-guard-contention-round-{round}");
            let mut record = usage_record(&row_id, round);
            record.created_at = Utc::now().to_rfc3339();
            usage_store.record(record).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            let soft_cutoff = Utc::now();

            let mut writer_tx = store.pool().begin().await.unwrap();
            acquire_usage_writer_commit_guard(&mut writer_tx)
                .await
                .unwrap();
            let started_at = std::time::Instant::now();
            let soft_contention = usage_store
                .soft_delete_cleanup_batch(soft_cutoff, 10)
                .await
                .unwrap();
            assert!(
                started_at.elapsed() < std::time::Duration::from_secs(1),
                "round {round}: soft cleanup contention must be bounded"
            );
            assert_eq!(soft_contention.processed_rows, 0, "round {round}");
            assert_eq!(soft_contention.has_remaining, Some(true), "round {round}");
            writer_tx.rollback().await.unwrap();

            let soft_recovered = usage_store
                .soft_delete_cleanup_batch(soft_cutoff, 10)
                .await
                .unwrap();
            assert_eq!(soft_recovered.processed_rows, 1, "round {round}");
            assert_eq!(soft_recovered.has_remaining, Some(false), "round {round}");

            let hard_cutoff = Utc::now() + chrono::Duration::seconds(1);
            let mut writer_tx = store.pool().begin().await.unwrap();
            acquire_usage_writer_commit_guard(&mut writer_tx)
                .await
                .unwrap();
            let started_at = std::time::Instant::now();
            let hard_contention = usage_store
                .hard_delete_cleanup_batch(hard_cutoff, 10)
                .await
                .unwrap();
            assert!(
                started_at.elapsed() < std::time::Duration::from_secs(1),
                "round {round}: hard cleanup should use try-lock, got {:?}",
                started_at.elapsed()
            );
            assert_eq!(hard_contention.processed_rows, 0, "round {round}");
            assert_eq!(hard_contention.has_remaining, Some(true), "round {round}");
            writer_tx.rollback().await.unwrap();

            let hard_recovered = usage_store
                .hard_delete_cleanup_batch(hard_cutoff, 10)
                .await
                .unwrap();
            assert_eq!(hard_recovered.processed_rows, 1, "round {round}");
            assert_eq!(hard_recovered.has_remaining, Some(false), "round {round}");
        }

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn postgres_cleanup_watermark_lock_wait_is_bounded_and_recovers() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;
        let usage_store = PostgresUsageStore::new(store.clone());
        let cutoff = Utc::now();

        let mut writer_tx = store.pool().begin().await.unwrap();
        acquire_usage_writer_commit_guard(&mut writer_tx)
            .await
            .unwrap();
        let started_at = std::time::Instant::now();
        let error = usage_store
            .advance_soft_delete_cleanup_watermark(cutoff)
            .await
            .unwrap_err();
        let elapsed = started_at.elapsed();
        assert!(
            error
                .chain()
                .any(|cause| cause.to_string().contains("lock timeout")),
            "expected a PostgreSQL lock timeout, got: {error:#}"
        );
        assert!(
            elapsed >= std::time::Duration::from_millis(150)
                && elapsed < std::time::Duration::from_secs(1),
            "watermark lock wait was not bounded near the 250ms contract: {elapsed:?}"
        );

        writer_tx.rollback().await.unwrap();
        assert_eq!(
            usage_store
                .advance_soft_delete_cleanup_watermark(cutoff)
                .await
                .unwrap(),
            postgres_timestamp_precision(cutoff)
        );
        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_cleanup_prunes_high_cardinality_zero_rollups_for_three_rounds() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        let usage_store = PostgresUsageStore::new(store.clone());
        let base = DateTime::parse_from_rfc3339("2026-07-16T01:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        for round in 0..3 {
            clean(&store).await;
            let mut records = Vec::new();
            for index in 0..48 {
                let mut record = usage_record(
                    &format!("zero-rollup-old-round-{round}-record-{index}"),
                    10_000 + round * 100 + index,
                );
                record.created_at =
                    (base + chrono::Duration::milliseconds(index as i64)).to_rfc3339();
                record.conversation_id = Some(format!(
                    "zero-rollup-old-conversation-round-{round}-{index}"
                ));
                record.credential_id = Some((10_000 + round * 100 + index) as u64);
                record.credential_label = Some(format!("old-{round}-{index}@example.test"));
                record.duration_ms = (1_000 + index) as u64;
                records.push(record);
            }
            let new_id = format!("zero-rollup-new-round-{round}");
            let new_conversation = format!("zero-rollup-new-conversation-round-{round}");
            let new_credential = (90_000 + round) as u64;
            let new_cache_read = 20_000 + round;
            let new_duration = 222 + round as u64;
            let mut newer = usage_record(&new_id, new_cache_read);
            newer.created_at = (base + chrono::Duration::minutes(10)).to_rfc3339();
            newer.conversation_id = Some(new_conversation.clone());
            newer.credential_id = Some(new_credential);
            newer.credential_label = Some(format!("new-{round}@example.test"));
            newer.duration_ms = new_duration;
            records.push(newer);
            usage_store.record_batch(records).await.unwrap();

            let cutoff = base + chrono::Duration::minutes(5);
            let mut processed = 0;
            let mut batches = 0;
            loop {
                let result = usage_store
                    .soft_delete_cleanup_batch(cutoff, 7)
                    .await
                    .unwrap();
                processed += result.processed_rows;
                batches += 1;
                if result.has_remaining == Some(false) {
                    break;
                }
                assert!(batches < 20, "round {round}: cleanup did not converge");
            }
            assert_eq!(processed, 48, "round {round}");
            assert!(
                batches > 1,
                "round {round}: fixture must use multiple batches"
            );

            for table in [
                "usage_rollup_totals",
                "usage_rollup_time_buckets",
                "usage_credential_cost_summary",
                "usage_cache_read_totals",
                "usage_cache_read_rollup_time_buckets",
                "usage_duration_rollup_time_buckets",
            ] {
                let query = format!("SELECT COUNT(*)::bigint FROM {table} WHERE requests <= 0");
                let zero_rows: i64 = sqlx::query_scalar(&query)
                    .fetch_one(store.pool())
                    .await
                    .unwrap();
                assert_eq!(zero_rows, 0, "round {round}: {table} retained zero rows");
            }

            let active_ids: Vec<String> = sqlx::query_scalar(
                "SELECT id FROM usage_records WHERE deleted_at IS NULL ORDER BY id",
            )
            .fetch_all(store.pool())
            .await
            .unwrap();
            assert_eq!(active_ids, vec![new_id], "round {round}");
            let conversation_requests: i64 = sqlx::query_scalar(
                r#"
                SELECT requests
                FROM usage_rollup_totals
                WHERE dimension = 'conversation' AND dimension_key = $1
                "#,
            )
            .bind(&new_conversation)
            .fetch_one(store.pool())
            .await
            .unwrap();
            assert_eq!(conversation_requests, 1, "round {round}");
            let conversation_rows: i64 = sqlx::query_scalar(
                "SELECT COUNT(*)::bigint FROM usage_rollup_totals WHERE dimension = 'conversation'",
            )
            .fetch_one(store.pool())
            .await
            .unwrap();
            assert_eq!(conversation_rows, 1, "round {round}");
            let credential_requests: i64 = sqlx::query_scalar(
                "SELECT requests FROM usage_credential_cost_summary WHERE credential_id = $1",
            )
            .bind(new_credential as i64)
            .fetch_one(store.pool())
            .await
            .unwrap();
            assert_eq!(credential_requests, 1, "round {round}");
            let credential_rows: i64 =
                sqlx::query_scalar("SELECT COUNT(*)::bigint FROM usage_credential_cost_summary")
                    .fetch_one(store.pool())
                    .await
                    .unwrap();
            assert_eq!(credential_rows, 1, "round {round}");
            let cache_requests: i64 = sqlx::query_scalar(
                "SELECT requests FROM usage_cache_read_totals WHERE cache_read_input_tokens = $1",
            )
            .bind(new_cache_read)
            .fetch_one(store.pool())
            .await
            .unwrap();
            assert_eq!(cache_requests, 1, "round {round}");
            let duration_requests: i64 = sqlx::query_scalar(
                r#"
                SELECT requests
                FROM usage_duration_rollup_time_buckets
                WHERE bucket_start = date_trunc('hour', $1::timestamptz)
                  AND duration_ms = $2
                "#,
            )
            .bind(base)
            .bind(new_duration as i32)
            .fetch_one(store.pool())
            .await
            .unwrap();
            assert_eq!(duration_requests, 1, "round {round}");
            let global_bucket_requests: i64 = sqlx::query_scalar(
                r#"
                SELECT requests
                FROM usage_rollup_time_buckets
                WHERE bucket_start = date_trunc('hour', $1::timestamptz)
                  AND dimension = 'global'
                  AND dimension_key = 'all'
                "#,
            )
            .bind(base)
            .fetch_one(store.pool())
            .await
            .unwrap();
            assert_eq!(global_bucket_requests, 1, "round {round}");
        }

        store.drop_test_schema().await.unwrap();
    }

    #[test]
    fn usage_rollup_bucket_start_truncates_to_hour() {
        let created_at = DateTime::parse_from_rfc3339("2026-06-11T01:23:45.678Z")
            .unwrap()
            .with_timezone(&Utc);
        let bucket_start = usage_rollup_bucket_start(created_at);
        let expected = DateTime::parse_from_rfc3339("2026-06-11T01:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(bucket_start, expected);
    }

    #[test]
    fn startup_migration_sql_does_not_scan_usage_history_tables() {
        let startup_sql = [
            ("SCHEMA_SQL", SCHEMA_SQL),
            (
                "CREDENTIAL_STORAGE_REVISION_SQL",
                CREDENTIAL_STORAGE_REVISION_SQL,
            ),
            (
                "CREDENTIAL_RUNTIME_REVISION_SQL",
                CREDENTIAL_RUNTIME_REVISION_SQL,
            ),
            (
                "CREDENTIAL_RUNTIME_GENERATION_SQL",
                CREDENTIAL_RUNTIME_GENERATION_SQL,
            ),
            (
                "CREDENTIAL_RUNTIME_MUTATION_CLEANUP_SQL",
                CREDENTIAL_RUNTIME_MUTATION_CLEANUP_SQL,
            ),
            (
                "CREDENTIAL_STATS_DELTA_BATCH_SQL",
                CREDENTIAL_STATS_DELTA_BATCH_SQL,
            ),
        ];
        let forbidden = [
            "FROM usage_records",
            "UPDATE usage_records",
            "INSERT INTO usage_rollup",
            "INSERT INTO usage_cache",
            "INSERT INTO usage_duration",
            "INSERT INTO usage_credential",
            "CREATE INDEX IF NOT EXISTS idx_usage_records",
            "CREATE INDEX IF NOT EXISTS idx_usage_rollup",
            "CREATE INDEX IF NOT EXISTS idx_usage_cache",
            "CREATE INDEX IF NOT EXISTS idx_usage_duration",
        ];

        for (name, sql) in startup_sql {
            for forbidden in forbidden {
                assert!(
                    !sql.contains(forbidden),
                    "{name} must not contain historical usage scan/backfill/index statement: {forbidden}"
                );
            }
        }
    }

    #[tokio::test]
    async fn postgres_rejects_online_rollup_compression_config_before_connecting() {
        let mut config = Config::default();
        config.postgres.compress_usage_rollups_on_start = true;
        let error = PostgresStore::connect(&config).await.unwrap_err();
        let message = error.to_string();
        assert!(message.contains("compressUsageRollupsOnStart"));
        assert!(message.contains("maintenance usage-rollup-compression"));
        assert!(!message.contains("postgres.url"));
    }

    #[tokio::test]
    async fn postgres_usage_lifecycle_fence_requires_offline_maintenance() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let service_a = PostgresUsageLifecycleGuard::acquire_service(&config)
            .await
            .unwrap();
        let service_b = PostgresUsageLifecycleGuard::acquire_service(&config)
            .await
            .unwrap();
        let maintenance_error = PostgresUsageLifecycleGuard::acquire_offline_maintenance(&config)
            .await
            .err()
            .expect("online services must reject offline usage maintenance");
        assert!(
            maintenance_error
                .to_string()
                .contains("every gateway instance to be stopped")
        );

        service_a.release().await.unwrap();
        service_b.release().await.unwrap();
        let maintenance = PostgresUsageLifecycleGuard::acquire_offline_maintenance(&config)
            .await
            .unwrap();
        let service_error = PostgresUsageLifecycleGuard::acquire_service(&config)
            .await
            .err()
            .expect("offline maintenance must reject service startup");
        assert!(
            service_error
                .to_string()
                .contains("cannot start while offline usage maintenance")
        );

        maintenance.release().await.unwrap();
        PostgresUsageLifecycleGuard::acquire_service(&config)
            .await
            .unwrap()
            .release()
            .await
            .unwrap();
    }

    #[test]
    fn usage_legacy_cost_backfill_is_explicit_maintenance_sql() {
        for expected in [
            "UPDATE usage_records",
            "UPDATE usage_rollup_totals",
            "UPDATE usage_rollup_time_buckets",
            "UPDATE usage_credential_cost_summary",
        ] {
            assert!(
                USAGE_LEGACY_COST_FIELD_BACKFILL_SQL.contains(expected),
                "legacy cost backfill maintenance SQL should contain expected statement: {expected}"
            );
        }
    }

    async fn startup_migration_atomicity_snapshot(store: &PostgresStore) -> serde_json::Value {
        let columns: Vec<String> = sqlx::query_scalar(
            r#"
            SELECT concat_ws(
                ':', table_name, ordinal_position::text, column_name, data_type,
                is_nullable, COALESCE(column_default, '')
            )
            FROM information_schema.columns
            WHERE table_schema = current_schema()
            ORDER BY table_name, ordinal_position
            "#,
        )
        .fetch_all(store.pool())
        .await
        .unwrap();
        let tables: Vec<String> = sqlx::query_scalar(
            r#"
            SELECT concat_ws(':', table_name, table_type)
            FROM information_schema.tables
            WHERE table_schema = current_schema()
            ORDER BY table_name
            "#,
        )
        .fetch_all(store.pool())
        .await
        .unwrap();
        let indexes: Vec<String> = sqlx::query_scalar(
            r#"
            SELECT concat_ws(':', tablename, indexname, indexdef)
            FROM pg_indexes
            WHERE schemaname = current_schema()
            ORDER BY tablename, indexname
            "#,
        )
        .fetch_all(store.pool())
        .await
        .unwrap();
        let markers: Vec<String> = sqlx::query_scalar(
            r#"
            SELECT concat_ws(':', version, checksum, applied_at::text)
            FROM schema_migrations
            ORDER BY version
            "#,
        )
        .fetch_all(store.pool())
        .await
        .unwrap();
        let runtime_config: serde_json::Value = sqlx::query_scalar(
            "SELECT jsonb_build_object('config', config, 'version', version) FROM runtime_config WHERE id = 'current'",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        let credentials: Vec<serde_json::Value> = sqlx::query_scalar(
            r#"
            SELECT jsonb_build_object(
                'id', id,
                'priority', priority,
                'disabled', disabled,
                'auth_kind', auth_kind,
                'api_key_hash', api_key_hash,
                'refresh_token_hash', refresh_token_hash,
                'data', data,
                'deleted_at', deleted_at
            )
            FROM credentials
            ORDER BY id
            "#,
        )
        .fetch_all(store.pool())
        .await
        .unwrap();

        serde_json::json!({
            "columns": columns,
            "tables": tables,
            "indexes": indexes,
            "markers": markers,
            "runtimeConfig": runtime_config,
            "credentials": credentials,
        })
    }

    #[tokio::test]
    async fn postgres_startup_migration_checksum_failure_rolls_back_entire_default_chain() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;
        sqlx::query(
            r#"
            INSERT INTO runtime_config (id, config, version)
            VALUES ('current', '{"fixture":"migration-atomicity"}'::jsonb, 17)
            "#,
        )
        .execute(store.pool())
        .await
        .unwrap();
        store
            .insert_credential(&KiroCredentials {
                kiro_api_key: Some("ksk_migration_atomicity_fixture".to_string()),
                auth_method: Some("api_key".to_string()),
                priority: 23,
                email: Some("migration-atomicity@example.test".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();

        for statement in [
            "DROP TABLE credential_stats_delta_batches",
            "DROP TABLE credential_runtime_mutations",
            "ALTER TABLE credentials DROP COLUMN revision",
            "ALTER TABLE credential_runtime_state DROP COLUMN revision",
            "ALTER TABLE credential_runtime_state DROP COLUMN generation",
        ] {
            sqlx::query(statement).execute(store.pool()).await.unwrap();
        }
        sqlx::query(
            r#"
            DELETE FROM schema_migrations
            WHERE version IN (
                'credential-runtime-revision-v1',
                'credential-runtime-generation-v1',
                'credential-runtime-mutation-cleanup-v1',
                'credential-stats-delta-batches-v1'
            )
            "#,
        )
        .execute(store.pool())
        .await
        .unwrap();
        let updated = sqlx::query(
            r#"
            UPDATE schema_migrations
            SET checksum = 'fixture-corrupt-checksum'
            WHERE version = 'credential-storage-revision-v1'
            "#,
        )
        .execute(store.pool())
        .await
        .unwrap();
        assert_eq!(updated.rows_affected(), 1);

        let before = startup_migration_atomicity_snapshot(&store).await;
        for attempt in 1..=5 {
            let error = store.migrate_with_options(false).await.unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("credential-storage-revision-v1 checksum mismatch"),
                "attempt {attempt} returned unexpected error: {error:#}"
            );
            assert_eq!(
                startup_migration_atomicity_snapshot(&store).await,
                before,
                "failed startup migration attempt {attempt} left a partial schema, marker, or business-data mutation"
            );
        }

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_repeated_startup_migration_preserves_unchanged_inline_marker_time() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        let initial: (String, DateTime<Utc>) = sqlx::query_as(
            "SELECT checksum, applied_at FROM schema_migrations WHERE version = 'inline-schema'",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        store.migrate_with_options(false).await.unwrap();
        let repeated: (String, DateTime<Utc>) = sqlx::query_as(
            "SELECT checksum, applied_at FROM schema_migrations WHERE version = 'inline-schema'",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(repeated, initial);

        sqlx::query(
            r#"
            UPDATE schema_migrations
            SET checksum = 'fixture-stale-inline-checksum',
                applied_at = now() - interval '1 day'
            WHERE version = 'inline-schema'
            "#,
        )
        .execute(store.pool())
        .await
        .unwrap();
        let stale_applied_at: DateTime<Utc> = sqlx::query_scalar(
            "SELECT applied_at FROM schema_migrations WHERE version = 'inline-schema'",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();

        store.migrate_with_options(false).await.unwrap();
        let repaired: (String, DateTime<Utc>) = sqlx::query_as(
            "SELECT checksum, applied_at FROM schema_migrations WHERE version = 'inline-schema'",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(repaired.0, sha256_hex(SCHEMA_SQL));
        assert!(repaired.1 > stale_applied_at);

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        store.migrate_with_options(false).await.unwrap();
        let final_marker: (String, DateTime<Utc>) = sqlx::query_as(
            "SELECT checksum, applied_at FROM schema_migrations WHERE version = 'inline-schema'",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(final_marker, repaired);

        store.drop_test_schema().await.unwrap();
    }

    #[test]
    fn legacy_usage_record_cost_compatibility_falls_back_to_estimated_cost() {
        let mut record = usage_record("legacy-cost", 10);
        record.estimated_cost_usd = 0.42;
        record.original_cost_usd = 0.0;

        apply_usage_record_legacy_cost_compatibility(&mut record);

        assert_eq!(record.original_cost_usd, 0.42);
    }

    #[tokio::test]
    async fn postgres_usage_rollup_writes_hour_buckets() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;
        let usage_store = PostgresUsageStore::new(Arc::new(store.clone()));

        let mut first = usage_record("hour-bucket-1", 10);
        first.created_at = "2026-06-11T01:10:35Z".to_string();
        let mut second = usage_record("hour-bucket-2", 20);
        second.created_at = "2026-06-11T01:59:59Z".to_string();
        usage_store.record(first).await.unwrap();
        usage_store.record(second).await.unwrap();

        let row = sqlx::query(
            r#"
            SELECT COUNT(*)::bigint AS rows,
                   MIN(bucket_start) AS bucket_start,
                   SUM(requests)::bigint AS requests
            FROM usage_rollup_time_buckets
            WHERE dimension = 'global' AND dimension_key = 'all'
            "#,
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        let rows: i64 = row.try_get("rows").unwrap();
        let bucket_start: DateTime<Utc> = row.try_get("bucket_start").unwrap();
        let requests: i64 = row.try_get("requests").unwrap();
        assert_eq!(rows, 1);
        assert_eq!(
            bucket_start,
            DateTime::parse_from_rfc3339("2026-06-11T01:00:00Z")
                .unwrap()
                .with_timezone(&Utc)
        );
        assert_eq!(requests, 2);

        let cache_bucket_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(DISTINCT bucket_start)::bigint FROM usage_cache_read_rollup_time_buckets",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(cache_bucket_count, 1);

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_usage_concurrent_writers_keep_rollup_lock_order_stable() {
        let Some(mut config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        config.postgres.max_connections = 4;

        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;
        let left_store = PostgresUsageStore::new(store.clone());
        let right_store = PostgresUsageStore::new(store.clone());

        for round in 0..3 {
            let ids = vec![format!("missing-id-guard-{round}")];
            let mut holder = store.pool().begin().await.unwrap();
            acquire_usage_writer_commit_guard(&mut holder)
                .await
                .unwrap();
            acquire_usage_record_commit_guards(&mut holder, &ids)
                .await
                .unwrap();

            let waiting_store = store.clone();
            let waiting_ids = ids.clone();
            let waiter = tokio::spawn(async move {
                let mut tx = waiting_store.pool().begin().await?;
                acquire_usage_writer_commit_guard(&mut tx).await?;
                acquire_usage_record_commit_guards(&mut tx, &waiting_ids).await?;
                tx.commit().await?;
                Ok::<(), anyhow::Error>(())
            });
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            assert!(
                !waiter.is_finished(),
                "round {round}: a second writer crossed the missing-ID advisory guard"
            );
            holder.commit().await.unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(2), waiter)
                .await
                .expect("missing-ID waiter must recover after commit")
                .expect("missing-ID waiter task")
                .expect("missing-ID waiter transaction");
        }

        for round in 0..64 {
            let mut left = usage_record(&format!("concurrent-left-{round}"), round);
            left.conversation_id = Some(format!("left-session-{round}"));
            let mut right = usage_record(&format!("concurrent-right-{round}"), round + 1);
            right.conversation_id = Some(format!("right-session-{round}"));
            let gate = Arc::new(tokio::sync::Barrier::new(2));
            let left_gate = gate.clone();
            let right_gate = gate.clone();

            let (left_result, right_result) = tokio::join!(
                async {
                    left_gate.wait().await;
                    left_store.record_batch(vec![left]).await
                },
                async {
                    right_gate.wait().await;
                    right_store.record_batch(vec![right]).await
                },
            );
            assert!(
                left_result.is_ok(),
                "round {round}: left usage writer failed: {left_result:?}"
            );
            assert!(
                right_result.is_ok(),
                "round {round}: right usage writer failed: {right_result:?}"
            );
        }

        for round in 0..32 {
            let left_records = vec![
                usage_record("concurrent-shared-a", round),
                usage_record("concurrent-shared-b", round + 1),
            ];
            let right_records = vec![
                usage_record("concurrent-shared-b", round + 2),
                usage_record("concurrent-shared-a", round + 3),
            ];
            let gate = Arc::new(tokio::sync::Barrier::new(2));
            let left_gate = gate.clone();
            let right_gate = gate.clone();

            let (left_result, right_result) = tokio::join!(
                async {
                    left_gate.wait().await;
                    left_store.record_batch(left_records).await
                },
                async {
                    right_gate.wait().await;
                    right_store.record_batch(right_records).await
                },
            );
            assert!(
                left_result.is_ok(),
                "shared round {round}: left usage writer failed: {left_result:?}"
            );
            assert!(
                right_result.is_ok(),
                "shared round {round}: right usage writer failed: {right_result:?}"
            );
        }

        let requests: i64 = sqlx::query_scalar(
            "SELECT requests FROM usage_rollup_totals WHERE dimension = 'global' AND dimension_key = 'all'",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(requests, 130);

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_dashboard_duration_p95_uses_weighted_histogram_and_negative_deltas() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;
        let usage_store = PostgresUsageStore::new(store.clone());
        let base = fixed_utc("2026-07-15T10:00:00Z");
        let specs = [dashboard_test_window(
            "fixed",
            base,
            base + chrono::Duration::hours(2),
        )];

        let records = (1..=100)
            .map(|duration_ms| {
                let mut record = usage_record(&format!("duration-p95-{duration_ms}"), 0);
                record.created_at = (base
                    + chrono::Duration::hours(if duration_ms % 2 == 0 { 1 } else { 0 })
                    + chrono::Duration::seconds(duration_ms as i64))
                .to_rfc3339();
                record.duration_ms = duration_ms;
                record.response_latency_ms = Some(duration_ms);
                record
            })
            .collect::<Vec<_>>();
        usage_store.record_batch(records).await.unwrap();

        let windows = usage_store.dashboard_windows(&specs, 0).await.unwrap();
        assert_eq!(windows[0].summary.total_requests, 100);
        assert_eq!(windows[0].summary.p95_duration_ms, 95);
        assert!((windows[0].summary.average_duration_ms - 50.5).abs() < f64::EPSILON);

        let mut replacement = usage_record("duration-p95-100", 0);
        replacement.created_at =
            (base + chrono::Duration::hours(1) + chrono::Duration::seconds(1)).to_rfc3339();
        replacement.duration_ms = 1;
        replacement.response_latency_ms = Some(1);
        usage_store.record(replacement).await.unwrap();
        let windows = usage_store.dashboard_windows(&specs, 0).await.unwrap();
        assert_eq!(windows[0].summary.total_requests, 100);
        assert_eq!(windows[0].summary.p95_duration_ms, 94);

        clean(&store).await;
        let weighted = (0..100)
            .map(|index| {
                let mut record = usage_record(&format!("duration-weighted-{index}"), 0);
                record.created_at = (base
                    + chrono::Duration::hours(if index < 95 { 0 } else { 1 })
                    + chrono::Duration::seconds(index as i64))
                .to_rfc3339();
                record.duration_ms = if index < 95 { 10 } else { 1_000 };
                record.response_latency_ms = Some(record.duration_ms);
                record
            })
            .collect::<Vec<_>>();
        usage_store.record_batch(weighted).await.unwrap();
        let windows = usage_store.dashboard_windows(&specs, 0).await.unwrap();
        assert_eq!(windows[0].summary.total_requests, 100);
        assert_eq!(windows[0].summary.p95_duration_ms, 10);

        clean(&store).await;
        let weighted = (0..100)
            .map(|index| {
                let mut record = usage_record(&format!("duration-weighted-tail-{index}"), 0);
                record.created_at = (base
                    + chrono::Duration::hours(if index < 94 { 0 } else { 1 })
                    + chrono::Duration::seconds(index as i64))
                .to_rfc3339();
                record.duration_ms = if index < 94 { 10 } else { 1_000 };
                record.response_latency_ms = Some(record.duration_ms);
                record
            })
            .collect::<Vec<_>>();
        usage_store.record_batch(weighted).await.unwrap();
        let windows = usage_store.dashboard_windows(&specs, 0).await.unwrap();
        assert_eq!(windows[0].summary.total_requests, 100);
        assert_eq!(windows[0].summary.p95_duration_ms, 1_000);

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_dashboard_read_transaction_is_bounded_and_read_only() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let store = PostgresStore::connect_test(&config).await.unwrap();

        let mut tx = store.pool().begin().await.unwrap();
        configure_usage_dashboard_read_transaction(&mut tx)
            .await
            .unwrap();

        let statement_timeout: String = sqlx::query_scalar("SHOW statement_timeout")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        let lock_timeout: String = sqlx::query_scalar("SHOW lock_timeout")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        let read_only: String = sqlx::query_scalar("SHOW transaction_read_only")
            .fetch_one(&mut *tx)
            .await
            .unwrap();

        assert!(
            matches!(statement_timeout.as_str(), "115s" | "115000ms" | "1min 55s"),
            "unexpected dashboard statement_timeout: {statement_timeout}"
        );
        assert_eq!(lock_timeout, "250ms");
        assert_eq!(read_only, "on");

        tx.commit().await.unwrap();
        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_dashboard_uses_exact_utc_boundary_population_for_every_window_metric() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;
        let usage_store = PostgresUsageStore::new(store.clone());
        let base = fixed_utc("2026-07-15T10:00:00Z");

        let mut before = external_usage_record("dashboard-before", 0.010, 0.006, 0.008);
        before.created_at = (base + chrono::Duration::minutes(10)).to_rfc3339();

        let mut at_from = external_usage_record("dashboard-at-from", 0.010, 0.006, 0.008);
        at_from.created_at = (base + chrono::Duration::minutes(15)).to_rfc3339();
        at_from.kiro_metering_usage = 1.0;

        let mut first_boundary = usage_record("dashboard-first-boundary", 2_000);
        first_boundary.created_at = (base + chrono::Duration::minutes(30)).to_rfc3339();
        first_boundary.duration_ms = 30;
        first_boundary.kiro_metering_usage = 2.0;

        let mut before_full_hour = usage_record("dashboard-before-full-hour", 0);
        before_full_hour.created_at =
            (base + chrono::Duration::minutes(59) + chrono::Duration::seconds(59)).to_rfc3339();
        before_full_hour.status = UsageRecordStatus::Error;
        before_full_hour.usage_source = UsageSource::RequestEstimate;
        before_full_hour.duration_ms = 40;
        before_full_hour.kiro_metering_usage = 3.0;

        let mut full_hour_start = usage_record("dashboard-full-hour-start", 2_000);
        full_hour_start.created_at = (base + chrono::Duration::hours(1)).to_rfc3339();
        full_hour_start.duration_ms = 50;
        full_hour_start.kiro_metering_usage = 4.0;

        let mut full_hour_external =
            external_usage_record("dashboard-full-hour-external", 0.010, 0.006, 0.008);
        full_hour_external.created_at =
            (base + chrono::Duration::hours(1) + chrono::Duration::minutes(30)).to_rfc3339();
        full_hour_external.kiro_metering_usage = 5.0;

        let mut tail_legacy = usage_record("dashboard-tail-legacy", 2_000);
        tail_legacy.created_at =
            (base + chrono::Duration::hours(2) + chrono::Duration::minutes(30)).to_rfc3339();
        tail_legacy.status = UsageRecordStatus::StreamError;
        tail_legacy.usage_source = UsageSource::ContextEstimate;
        tail_legacy.estimated_cost_usd = 0.123;
        tail_legacy.original_cost_usd = 0.0;
        tail_legacy.duration_ms = 60;
        tail_legacy.kiro_metering_usage = 6.0;

        let mut at_to = external_usage_record("dashboard-at-to", 1.0, 0.5, 0.75);
        at_to.created_at =
            (base + chrono::Duration::hours(2) + chrono::Duration::minutes(45)).to_rfc3339();

        usage_store
            .record_batch(vec![
                before,
                at_from,
                first_boundary,
                before_full_hour,
                full_hour_start,
                full_hour_external,
                tail_legacy,
                at_to,
            ])
            .await
            .unwrap();

        let specs = vec![
            dashboard_test_window(
                "same-hour",
                base + chrono::Duration::minutes(15),
                base + chrono::Duration::minutes(45),
            ),
            dashboard_test_window(
                "cross-hour",
                base + chrono::Duration::minutes(15),
                base + chrono::Duration::hours(2) + chrono::Duration::minutes(45),
            ),
            dashboard_test_window(
                "exact-hour",
                base + chrono::Duration::hours(1),
                base + chrono::Duration::hours(2),
            ),
            dashboard_test_window(
                "half-hour",
                base + chrono::Duration::minutes(30),
                base + chrono::Duration::hours(1) + chrono::Duration::minutes(30),
            ),
        ];

        let windows = usage_store.dashboard_windows(&specs, 1_000).await.unwrap();
        let by_key = windows
            .iter()
            .map(|window| (window.key.as_str(), window))
            .collect::<HashMap<_, _>>();
        assert_eq!(by_key["same-hour"].summary.total_requests, 2);
        assert_eq!(by_key["same-hour"].summary.high_cache_requests, 2);
        assert_eq!(by_key["same-hour"].summary.p95_duration_ms, 50);
        assert_eq!(by_key["cross-hour"].summary.total_requests, 6);
        assert_eq!(by_key["cross-hour"].summary.success_requests, 4);
        assert_eq!(by_key["cross-hour"].summary.error_requests, 2);
        assert_eq!(by_key["cross-hour"].summary.high_cache_requests, 5);
        assert_eq!(by_key["cross-hour"].summary.p95_duration_ms, 60);
        assert!((by_key["cross-hour"].summary.total_estimated_cost_usd - 0.142).abs() < 1e-12);
        assert!(
            (by_key["cross-hour"].summary.total_original_cost_usd - 0.146).abs() < 1e-12,
            "the boundary detail segment must preserve the legacy hourly original-cost fallback"
        );
        assert!((by_key["cross-hour"].summary.total_kiro_metering_usage - 21.0).abs() < 1e-12);
        assert_eq!(by_key["exact-hour"].summary.total_requests, 2);
        assert_eq!(by_key["exact-hour"].summary.high_cache_requests, 2);
        assert!((by_key["exact-hour"].summary.total_kiro_metering_usage - 9.0).abs() < 1e-12);
        assert_eq!(by_key["half-hour"].summary.total_requests, 3);
        assert!((by_key["half-hour"].summary.total_kiro_metering_usage - 9.0).abs() < 1e-12);

        let series = usage_store.dashboard_series(&specs).await.unwrap();
        let series_by_key = series
            .iter()
            .map(|point| (point.key.as_str(), point))
            .collect::<HashMap<_, _>>();
        assert_eq!(series_by_key["same-hour"].requests, 2);
        assert_eq!(series_by_key["cross-hour"].requests, 6);
        assert!((series_by_key["cross-hour"].total_kiro_metering_usage - 21.0).abs() < 1e-12);
        assert_eq!(series_by_key["exact-hour"].requests, 2);
        assert_eq!(series_by_key["half-hour"].requests, 3);

        let top_credentials = usage_store
            .dashboard_top_aggregates_for_window(&[specs[1].clone()], DashboardTopGroup::Credential)
            .await
            .unwrap();
        assert_eq!(top_credentials.items[0].key, "7");
        assert!(
            (top_credentials.items[0].total_kiro_metering_usage - 15.0).abs() < 1e-12,
            "windowed credential top must preserve Kiro metering from rollup and boundary rows"
        );

        let status = usage_store
            .dashboard_breakdown(&specs, DashboardBreakdownColumn::Status)
            .await
            .unwrap();
        let cross_status = &status["cross-hour"];
        assert_eq!(
            cross_status.iter().map(|item| item.requests).sum::<usize>(),
            6
        );
        assert!(
            cross_status
                .iter()
                .all(|item| (item.ratio - item.requests as f64 / 6.0).abs() < f64::EPSILON)
        );

        let sources = usage_store
            .dashboard_breakdown(&specs, DashboardBreakdownColumn::UsageSource)
            .await
            .unwrap();
        let cross_sources = &sources["cross-hour"];
        assert_eq!(
            cross_sources
                .iter()
                .map(|item| item.requests)
                .sum::<usize>(),
            6
        );
        assert_eq!(
            cross_sources
                .iter()
                .find(|item| item.key == "upstream_metadata")
                .map(|item| item.requests),
            Some(2)
        );

        let billing = usage_store
            .dashboard_external_pool_billing_by_pool(&specs)
            .await
            .unwrap();
        let cross_billing = &billing["cross-hour"];
        assert_eq!(cross_billing.len(), 1);
        assert_eq!(cross_billing[0].pool_id, 42);
        assert_eq!(cross_billing[0].requests, 2);
        assert_eq!(cross_billing[0].priced_requests, 2);
        assert_eq!(cross_billing[0].cost_floor_applied_requests, 2);
        assert!((cross_billing[0].raw_cost_usd - 0.020).abs() < 1e-12);
        assert!((cross_billing[0].shaped_cost_usd - 0.012).abs() < 1e-12);
        assert!((cross_billing[0].uplifted_cost_usd - 0.016).abs() < 1e-12);
        assert!((cross_billing[0].profit_usd + 0.004).abs() < 1e-12);
        assert_eq!(
            by_key["cross-hour"].summary.external_pool_billing.requests,
            cross_billing[0].requests
        );

        let (_, half_hour_offset) = usage_dashboard_timezone(Some("UTC+05:30"));
        let timezone_windows =
            usage_dashboard_windows(fixed_utc("2026-07-16T00:20:00Z"), half_hour_offset);
        assert_eq!(
            timezone_windows
                .iter()
                .find(|window| window.key == "today")
                .unwrap()
                .from,
            fixed_utc("2026-07-15T18:30:00Z")
        );

        let mut moved = usage_record("dashboard-first-boundary", 2_000);
        moved.created_at = (base - chrono::Duration::hours(1)).to_rfc3339();
        moved.duration_ms = 30;
        usage_store.record(moved).await.unwrap();
        let windows = usage_store.dashboard_windows(&specs, 1_000).await.unwrap();
        assert_eq!(
            windows
                .iter()
                .find(|window| window.key == "cross-hour")
                .unwrap()
                .summary
                .total_requests,
            5,
            "same-ID replacement must subtract the old boundary population"
        );

        let cleanup_cutoff = base + chrono::Duration::hours(1) + chrono::Duration::minutes(1);
        loop {
            let result = usage_store
                .soft_delete_cleanup_batch(cleanup_cutoff, 100)
                .await
                .unwrap();
            if result.has_remaining == Some(false) || result.processed_rows == 0 {
                break;
            }
        }
        let windows = usage_store.dashboard_windows(&specs, 1_000).await.unwrap();
        let by_key = windows
            .iter()
            .map(|window| (window.key.as_str(), window))
            .collect::<HashMap<_, _>>();
        assert_eq!(by_key["cross-hour"].summary.total_requests, 2);
        assert_eq!(by_key["exact-hour"].summary.total_requests, 1);

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_dashboard_applies_legacy_cost_fallback_after_utc_hour_regrouping() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;
        let usage_store = PostgresUsageStore::new(store.clone());
        let base = fixed_utc("2026-07-15T10:00:00Z");

        for (offset_minutes, estimated, original) in
            [(10, 1.0_f64, 0.0_f64), (40, 2.0_f64, 4.0_f64)]
        {
            sqlx::query(
                r#"
                INSERT INTO usage_rollup_time_buckets (
                    bucket_start, dimension, dimension_key, requests,
                    total_estimated_cost_usd, total_original_cost_usd
                )
                VALUES ($1, 'global', 'all', 1, $2, $3)
                "#,
            )
            .bind(base + chrono::Duration::minutes(offset_minutes))
            .bind(estimated)
            .bind(original)
            .execute(store.pool())
            .await
            .unwrap();
        }
        for (offset_minutes, raw, shaped, uplifted, profit, reported) in [
            (10, 0.5_f64, 0.0_f64, 0.0_f64, 0.0_f64, 1.0_f64),
            (40, 1.0_f64, 4.0_f64, 6.0_f64, 5.0_f64, 2.0_f64),
        ] {
            sqlx::query(
                r#"
                INSERT INTO usage_rollup_time_buckets (
                    bucket_start, dimension, dimension_key, dimension_label,
                    requests, external_pool_requests, external_pool_priced_requests,
                    external_pool_raw_cost_usd, external_pool_shaped_cost_usd,
                    external_pool_uplifted_cost_usd, external_pool_profit_usd,
                    external_pool_reported_cost_usd, external_pool_billable_cost_usd
                )
                VALUES ($1, 'external_pool', '42', 'legacy-pool',
                        1, 1, 1, $2, $3, $4, $5, $6, $4)
                "#,
            )
            .bind(base + chrono::Duration::minutes(offset_minutes))
            .bind(raw)
            .bind(shaped)
            .bind(uplifted)
            .bind(profit)
            .bind(reported)
            .execute(store.pool())
            .await
            .unwrap();
        }

        let specs = [dashboard_test_window(
            "fixed",
            base,
            base + chrono::Duration::hours(1),
        )];
        let windows = usage_store.dashboard_windows(&specs, 0).await.unwrap();
        assert_eq!(windows[0].summary.total_requests, 2);
        assert!((windows[0].summary.total_estimated_cost_usd - 3.0).abs() < 1e-12);
        assert_eq!(
            windows[0].summary.total_original_cost_usd, 4.0,
            "fallback must be evaluated after old sub-hour buckets are regrouped"
        );

        let billing = usage_store
            .dashboard_external_pool_billing_by_pool(&specs)
            .await
            .unwrap();
        let pool = &billing["fixed"][0];
        assert_eq!(pool.requests, 2);
        assert_eq!(pool.raw_cost_usd, 1.5);
        assert_eq!(pool.shaped_cost_usd, 4.0);
        assert_eq!(pool.uplifted_cost_usd, 6.0);
        assert_eq!(pool.profit_usd, 5.0);

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_duration_write_and_dashboard_histogram_saturate_without_signed_wrap() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;
        let usage_store = PostgresUsageStore::new(store.clone());
        let base = fixed_utc("2026-07-15T10:00:00Z");
        let mut record = usage_record("duration-u64-max", 0);
        record.created_at = (base + chrono::Duration::minutes(1)).to_rfc3339();
        record.duration_ms = u64::MAX;
        record.response_latency_ms = Some(u64::MAX);
        usage_store.record(record).await.unwrap();

        let stored_duration: i64 =
            sqlx::query_scalar("SELECT duration_ms FROM usage_records WHERE id = $1")
                .bind("duration-u64-max")
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(stored_duration, i64::MAX);

        let specs = [dashboard_test_window(
            "fixed",
            base,
            base + chrono::Duration::hours(1),
        )];
        let windows = usage_store.dashboard_windows(&specs, 0).await.unwrap();
        assert_eq!(windows[0].summary.total_requests, 1);
        assert_eq!(windows[0].summary.p95_duration_ms, i32::MAX as u64);
        assert!(windows[0].summary.average_duration_ms > i64::MAX as f64 * 0.99);

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_migration_compresses_second_rollup_buckets_to_hours() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;
        sqlx::query(
            "DELETE FROM schema_migrations WHERE version = 'usage-rollup-hour-bucket-compression-v1'",
        )
        .execute(store.pool())
        .await
        .unwrap();
        for (created_at, requests) in [
            ("2026-06-11T01:10:35Z", 1i64),
            ("2026-06-11T01:42:05Z", 2i64),
        ] {
            let bucket_start = DateTime::parse_from_rfc3339(created_at)
                .unwrap()
                .with_timezone(&Utc);
            sqlx::query(
                r#"
                INSERT INTO usage_rollup_time_buckets (
                    bucket_start, dimension, dimension_key, requests,
                    duration_ms_sum, duration_ms_count, duration_ms_max
                )
                VALUES ($1, 'global', 'all', $2, $3, $2, $4)
                "#,
            )
            .bind(bucket_start)
            .bind(requests)
            .bind(requests * 100)
            .bind(requests * 100)
            .execute(store.pool())
            .await
            .unwrap();
            sqlx::query(
                r#"
                INSERT INTO usage_cache_read_rollup_time_buckets (
                    bucket_start, cache_read_input_tokens, requests
                )
                VALUES ($1, 1000, $2)
                "#,
            )
            .bind(bucket_start)
            .bind(requests)
            .execute(store.pool())
            .await
            .unwrap();
            sqlx::query(
                r#"
                INSERT INTO usage_duration_rollup_time_buckets (
                    bucket_start, duration_ms, requests
                )
                VALUES ($1, 250, $2)
                "#,
            )
            .bind(bucket_start)
            .bind(requests)
            .execute(store.pool())
            .await
            .unwrap();
        }

        store.migrate_with_options(false).await.unwrap();
        let default_rows: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*)::bigint
            FROM usage_rollup_time_buckets
            WHERE dimension = 'global' AND dimension_key = 'all'
            "#,
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(default_rows, 2);
        let default_applied: Option<String> =
            sqlx::query_scalar("SELECT checksum FROM schema_migrations WHERE version = $1")
                .bind("usage-rollup-hour-bucket-compression-v1")
                .fetch_optional(store.pool())
                .await
                .unwrap();
        assert!(default_applied.is_none());

        let mut writer = store.pool().begin().await.unwrap();
        acquire_usage_writer_commit_guard(&mut writer)
            .await
            .unwrap();
        let compression_store = store.clone();
        let compression = tokio::spawn(async move {
            compression_store
                .compress_usage_rollups_to_hour_buckets()
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !compression.is_finished(),
            "rollup compression must wait for an active usage writer"
        );
        writer.commit().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), compression)
            .await
            .expect("rollup compression must recover after writer commit")
            .expect("rollup compression task")
            .expect("rollup compression result");

        let row = sqlx::query(
            r#"
            SELECT COUNT(*)::bigint AS rows,
                   MIN(bucket_start) AS bucket_start,
                   SUM(requests)::bigint AS requests,
                   SUM(duration_ms_sum)::bigint AS duration_ms_sum,
                   MAX(duration_ms_max)::bigint AS duration_ms_max
            FROM usage_rollup_time_buckets
            WHERE dimension = 'global' AND dimension_key = 'all'
            "#,
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        let rows: i64 = row.try_get("rows").unwrap();
        let bucket_start: DateTime<Utc> = row.try_get("bucket_start").unwrap();
        let requests: i64 = row.try_get("requests").unwrap();
        let duration_ms_sum: i64 = row.try_get("duration_ms_sum").unwrap();
        let duration_ms_max: i64 = row.try_get("duration_ms_max").unwrap();
        assert_eq!(rows, 1);
        assert_eq!(
            bucket_start,
            DateTime::parse_from_rfc3339("2026-06-11T01:00:00Z")
                .unwrap()
                .with_timezone(&Utc)
        );
        assert_eq!(requests, 3);
        assert_eq!(duration_ms_sum, 300);
        assert_eq!(duration_ms_max, 200);

        let cache_requests: i64 = sqlx::query_scalar(
            "SELECT SUM(requests)::bigint FROM usage_cache_read_rollup_time_buckets",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(cache_requests, 3);
        let cache_rows: i64 =
            sqlx::query_scalar("SELECT COUNT(*)::bigint FROM usage_cache_read_rollup_time_buckets")
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(cache_rows, 1);

        let duration_requests: i64 = sqlx::query_scalar(
            "SELECT SUM(requests)::bigint FROM usage_duration_rollup_time_buckets",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(duration_requests, 3);
        let duration_rows: i64 =
            sqlx::query_scalar("SELECT COUNT(*)::bigint FROM usage_duration_rollup_time_buckets")
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(duration_rows, 1);

        store.drop_test_schema().await.unwrap();
    }

    fn external_usage_snapshot(
        input_tokens: i32,
        output_tokens: i32,
        cache_read_input_tokens: i32,
        cache_creation_input_tokens: i32,
    ) -> ExternalPoolUsageSnapshot {
        ExternalPoolUsageSnapshot {
            total_input_tokens: input_tokens
                .saturating_add(cache_read_input_tokens)
                .saturating_add(cache_creation_input_tokens),
            input_tokens,
            billable_input_tokens: input_tokens.saturating_add(cache_creation_input_tokens),
            output_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
            cache_creation_5m_input_tokens: cache_creation_input_tokens,
            cache_creation_1h_input_tokens: 0,
        }
    }

    fn external_usage_record(
        id: &str,
        raw_cost_usd: f64,
        shaped_cost_usd: f64,
        uplifted_cost_usd: f64,
    ) -> UsageRecord {
        let raw_usage = external_usage_snapshot(10_000, 100, 0, 2_000);
        let shaped_usage = if shaped_cost_usd < raw_cost_usd {
            external_usage_snapshot(200, 100, 9_800, 2_000)
        } else {
            external_usage_snapshot(12_000, 100, 0, 0)
        };
        let reported_usage = if uplifted_cost_usd >= shaped_cost_usd {
            external_usage_snapshot(250, 100, 12_250, 2_000)
        } else {
            shaped_usage
        };
        let profit_usd = uplifted_cost_usd - raw_cost_usd;
        UsageRecord {
            id: id.to_string(),
            created_at: Utc::now().to_rfc3339(),
            endpoint: "/cc/v1/messages".to_string(),
            stream: id.ends_with('0'),
            model: "claude-sonnet-4-5".to_string(),
            requested_max_tokens: None,
            downstream_stop_reason: None,
            upstream_model: Some("claude-sonnet-4-5".to_string()),
            external_outbound_model: Some("claude-sonnet-4-5".to_string()),
            model_resolution_source: Some("exact".to_string()),
            model_resolution_note: None,
            conversation_id: Some(format!("external-session-{}", id)),
            request_api_key_id: Some("request-key-external".to_string()),
            credential_id: None,
            credential_label: None,
            status: UsageRecordStatus::Success,
            usage_source: UsageSource::UpstreamMetadata,
            raw_usage: Some(raw_usage),
            total_input_tokens: reported_usage.total_input_tokens,
            compat_input_tokens: reported_usage.input_tokens,
            billable_input_tokens: reported_usage.billable_input_tokens,
            output_tokens: reported_usage.output_tokens,
            cache_read_input_tokens: reported_usage.cache_read_input_tokens,
            cache_creation_input_tokens: reported_usage.cache_creation_input_tokens,
            cache_creation_5m_input_tokens: reported_usage.cache_creation_5m_input_tokens,
            cache_creation_1h_input_tokens: reported_usage.cache_creation_1h_input_tokens,
            estimated_cost_usd: uplifted_cost_usd,
            original_cost_usd: raw_cost_usd,
            kiro_metering_usage: 0.0,
            pricing_available: true,
            pricing_model: Some("claude-sonnet-4-5".to_string()),
            duration_ms: 50,
            first_token_latency_ms: None,
            response_latency_ms: Some(50),
            latency_trace: None,
            simulated: false,
            sticky_bound: false,
            fallback_from_sticky: false,
            route_kind: Some(UsageRouteKind::ExternalPool),
            route_subtype: Some(UsageRouteSubtype::ExternalFallbackAfterLocalAttempts),
            fallback_reason: Some("local_transient_exhausted".to_string()),
            direct_policy_reason: None,
            local_attempted: Some(true),
            local_preflight: None,
            external_pool_id: Some(42),
            external_pool_name: Some("backup-a".to_string()),
            external_attempts: Vec::new(),
            usage_projection_applied: Some(true),
            external_pool_billing: Some(ExternalPoolBilling {
                request_input_tokens: None,
                raw_usage,
                shaped_usage,
                reported_usage,
                usage_projection_applied: true,
                raw_cost_usd,
                shaped_cost_usd,
                uplifted_cost_usd,
                profit_usd,
                reported_cost_usd: uplifted_cost_usd,
                billable_cost_usd: uplifted_cost_usd,
                cost_floor_delta_usd: (raw_cost_usd - uplifted_cost_usd).max(0.0),
                cost_floor_applied: uplifted_cost_usd < raw_cost_usd,
                pricing_available: true,
                pricing_model: Some("claude-sonnet-4-5".to_string()),
                usage_projection_mode: "current_path_policy".to_string(),
                stream_response_mode: None,
                usage_estimated: false,
                usage_estimate_reason: None,
                usage_candidate_path: None,
                body_usage_projection_applied: true,
            }),
            credential_attempts: Vec::new(),
            error_type: None,
            error_message: None,
            error_detail: None,
            error_status_code: None,
            error_source: None,
            error_id: None,
            error_metadata: None,
            raw_upstream_error: None,
            public_error_status_code: None,
            public_error_type: None,
            public_error_message: None,
            payload_breakdown: None,
            payload_guard_report: None,
        }
    }

    #[tokio::test]
    async fn postgres_persists_runtime_config_credentials_stats_usage_and_pricing() {
        let Some(mut config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;

        config.load_balancing_mode = "balanced".to_string();
        config.reported_usage.path_overrides.insert(
            "/custom".to_string(),
            ReportedUsagePathPolicy {
                input: ReportedUsageFieldPolicy::sample_input_max(42),
                ..ReportedUsagePathPolicy::default()
            },
        );
        store.save_runtime_config(&config).await.unwrap();
        let loaded_config = store.load_runtime_config().await.unwrap().unwrap();
        assert_eq!(loaded_config.load_balancing_mode, "balanced");
        assert_eq!(
            loaded_config
                .reported_usage
                .policy_for_path("/custom/v1/messages")
                .input
                .max_tokens,
            42
        );

        let credential = KiroCredentials {
            id: Some(7),
            email: Some("alpha@example.com".to_string()),
            refresh_token: Some("refresh".to_string()),
            priority: 3,
            ..Default::default()
        };
        store
            .bootstrap_credentials_from_file(std::slice::from_ref(&credential))
            .await
            .unwrap();
        let credentials = store.load_credentials().await.unwrap();
        assert_eq!(credentials.len(), 1);
        assert_eq!(credentials[0].id, Some(7));
        assert_eq!(credentials[0].email.as_deref(), Some("alpha@example.com"));
        store
            .save_credential_account_info(
                7,
                &CredentialAccountInfoRow {
                    subscription_title: Some("Kiro Pro".to_string()),
                    current_usage: 90.0,
                    usage_limit: 1000.0,
                    remaining: 910.0,
                    usage_percentage: 9.0,
                    credit_limit: 11_000.0,
                    credit_remaining: 10_910.0,
                    credit_base: 1_000.0,
                    credit_bonus: 10_000.0,
                    overage_status: Some("ENABLED".to_string()),
                    overage_capability: Some("OVERAGE_CAPABLE".to_string()),
                    overage_cap: 10.0,
                    overage_rate: 0.04,
                    current_overages: 0.0,
                    next_reset_at: Some(1_780_000_000.0),
                    checked_at: Utc::now().to_rfc3339(),
                },
            )
            .await
            .unwrap();
        let account_info = store.load_credential_account_info().await.unwrap();
        let account_info = account_info.get(&7).unwrap();
        assert_eq!(account_info.subscription_title.as_deref(), Some("Kiro Pro"));
        assert_eq!(account_info.current_usage, 90.0);
        assert_eq!(account_info.usage_limit, 1000.0);

        let inserted = store
            .insert_credential(&KiroCredentials {
                email: Some("beta@example.com".to_string()),
                kiro_api_key: Some("ksk_beta_key".to_string()),
                auth_method: Some("api_key".to_string()),
                priority: 1,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(inserted.id.unwrap() >= 8);
        assert_eq!(inserted.disabled, false);
        let credentials = store.load_credentials().await.unwrap();
        assert_eq!(credentials.len(), 2);
        store
            .save_credentials(std::slice::from_ref(&credential))
            .await
            .unwrap();
        let credentials = store.load_credentials().await.unwrap();
        assert_eq!(
            credentials.len(),
            2,
            "保存旧快照不应软删除数据库中其他未软删除凭据"
        );
        store
            .save_credentials(&[KiroCredentials {
                email: Some("gamma@example.com".to_string()),
                kiro_api_key: Some("ksk_gamma_key".to_string()),
                auth_method: Some("api_key".to_string()),
                priority: 2,
                ..Default::default()
            }])
            .await
            .unwrap();
        let credentials = store.load_credentials().await.unwrap();
        assert_eq!(credentials.len(), 3);
        assert!(
            credentials
                .iter()
                .any(
                    |credential| credential.email.as_deref() == Some("gamma@example.com")
                        && credential.id.is_some()
                )
        );

        let stats = HashMap::from([(
            7,
            CredentialStatsRow {
                success_count: 9,
                selection_count: 11,
                last_used_at: Some("2026-05-24T00:00:00Z".to_string()),
            },
        )]);
        store.save_credential_stats(&stats).await.unwrap();
        let loaded_stats = store.load_credential_stats().await.unwrap();
        assert_eq!(loaded_stats.get(&7).unwrap().success_count, 9);
        assert_eq!(loaded_stats.get(&7).unwrap().selection_count, 11);
        store.record_credential_selection(7).await.unwrap();
        let loaded_stats = store.load_credential_stats().await.unwrap();
        assert_eq!(loaded_stats.get(&7).unwrap().selection_count, 12);
        store
            .save_credential_stats(&HashMap::from([(
                7,
                CredentialStatsRow {
                    success_count: 10,
                    selection_count: 3,
                    last_used_at: Some("2026-05-24T00:01:00Z".to_string()),
                },
            )]))
            .await
            .unwrap();
        let loaded_stats = store.load_credential_stats().await.unwrap();
        assert_eq!(loaded_stats.get(&7).unwrap().selection_count, 12);

        let runtime_state = HashMap::from([(
            7,
            CredentialRuntimeStateRow {
                failure_count: 2,
                refresh_failure_count: 1,
                disabled_reason: Some("TooManyFailures".to_string()),
                warmup_remaining: 4,
                revision: 0,
                ..Default::default()
            },
        )]);
        store
            .save_credential_runtime_state(&runtime_state)
            .await
            .unwrap();
        let loaded_runtime_state = store.load_credential_runtime_state().await.unwrap();
        let loaded_runtime_state = loaded_runtime_state.get(&7).unwrap();
        assert_eq!(loaded_runtime_state.failure_count, 2);
        assert_eq!(loaded_runtime_state.refresh_failure_count, 1);
        assert_eq!(
            loaded_runtime_state.disabled_reason.as_deref(),
            Some("TooManyFailures")
        );
        assert_eq!(loaded_runtime_state.warmup_remaining, 4);
        assert_eq!(loaded_runtime_state.revision, 1);
        let (loaded_credentials, loaded_runtime_states, loaded_account_info) = store
            .load_credentials_with_runtime_state_and_account_info()
            .await
            .unwrap();
        assert!(
            loaded_credentials
                .iter()
                .any(|credential| credential.id == Some(7))
        );
        assert_eq!(loaded_runtime_states.get(&7).unwrap().failure_count, 2);
        assert_eq!(
            loaded_account_info
                .get(&7)
                .unwrap()
                .overage_status
                .as_deref(),
            Some("ENABLED")
        );

        let usage_store = PostgresUsageStore::new(Arc::new(store.clone()));
        let mut usage_1 = usage_record("usage-1", 10);
        usage_1.kiro_metering_usage = 0.125;
        usage_store.record(usage_1).await.unwrap();
        let mut usage_2 = usage_record("usage-2", 20);
        usage_2.status = UsageRecordStatus::Error;
        usage_2.model = "claude-opus-4-5".to_string();
        usage_2.conversation_id = Some("session-b".to_string());
        usage_2.request_api_key_id = Some("request-key-b".to_string());
        usage_2.estimated_cost_usd = 0.0;
        usage_2.kiro_metering_usage = 0.375;
        usage_2.pricing_available = false;
        usage_2.pricing_model = None;
        usage_2.error_message = Some("upstream quota exceeded".to_string());
        usage_store.record(usage_2).await.unwrap();
        let page = usage_store
            .query_page(
                UsageRecordQuery {
                    min_cache_read: Some(10),
                    ..Default::default()
                },
                1,
                1,
            )
            .await
            .unwrap();
        assert_eq!(page.records.len(), 1);
        assert!(page.has_next);
        let queried = usage_store
            .query(UsageRecordQuery {
                q: Some("alpha@example.com".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(queried.total, 2);
        let error_query = usage_store
            .query(UsageRecordQuery {
                q: Some("quota".to_string()),
                status: Some(UsageRecordStatus::Error),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(error_query.total, 1);
        assert_eq!(error_query.records[0].id, "usage-2");
        let request_key_query = usage_store
            .query(UsageRecordQuery {
                request_api_key_id: Some("request-key-b".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(request_key_query.total, 1);
        assert_eq!(request_key_query.records[0].id, "usage-2");

        let summary = usage_store.summary(15).await.unwrap();
        assert_eq!(summary.total_requests, 2);
        assert_eq!(summary.success_requests, 1);
        assert_eq!(summary.error_requests, 1);
        assert_eq!(summary.high_cache_requests, 1);
        assert_eq!(summary.local_prompt_cache_requests, 2);
        assert_eq!(summary.priced_requests, 1);
        assert_eq!(summary.unpriced_requests, 1);
        assert_eq!(summary.realtime.window_seconds, REALTIME_USAGE_WINDOW_SECS);
        assert_eq!(summary.realtime.requests, 2);
        assert_eq!(summary.realtime.success_requests, 1);
        assert_eq!(summary.realtime.error_requests, 1);
        assert_eq!(summary.realtime.rpm, 2.0);
        assert_eq!(summary.realtime.success_rpm, 1.0);
        assert_eq!(summary.realtime.error_rpm, 1.0);
        assert_eq!(summary.realtime.total_tpm, 240.0);
        assert_eq!(summary.realtime.billable_tpm, 60.0);
        assert_eq!(summary.top_credentials[0].key, "7");
        assert_eq!(summary.top_conversations.len(), 2);

        let dashboard = usage_store
            .dashboard(Some("Asia/Shanghai"), 15)
            .await
            .unwrap();
        let today = dashboard
            .windows
            .iter()
            .find(|window| window.key == "today")
            .unwrap();
        assert_eq!(today.summary.total_requests, 2);
        assert_eq!(today.summary.error_requests, 1);
        assert_eq!(today.summary.high_cache_requests, 1);
        assert_eq!(today.summary.status_breakdown.len(), 2);
        assert_eq!(dashboard.series.hourly_24h.len(), 24);
        assert_eq!(dashboard.series.daily_7d.len(), 7);
        assert_eq!(dashboard.top.models.len(), 2);
        assert_eq!(dashboard.top.credentials[0].key, "7");
        assert_eq!(dashboard.top.errors[0].requests, 1);

        let cost_summary = usage_store.credential_cost_summary().await.unwrap();
        let cost_summary = cost_summary.get(&7).unwrap();
        assert_eq!(cost_summary.priced_requests, 1);
        assert_eq!(cost_summary.unpriced_requests, 1);
        assert!((cost_summary.estimated_cost_usd - 0.001).abs() < f64::EPSILON);
        assert!((cost_summary.kiro_metering_usage - 0.5).abs() < f64::EPSILON);

        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        let cleanup_cutoff = Utc::now();
        let cleanup = usage_store
            .soft_delete_cleanup_batch(cleanup_cutoff, 10)
            .await
            .unwrap();
        assert_eq!(cleanup.processed_rows, 2);
        assert_eq!(cleanup.has_remaining, Some(false));
        let cleared_summary = usage_store.summary(15).await.unwrap();
        assert_eq!(cleared_summary.total_requests, 0);
        assert_eq!(cleared_summary.success_requests, 0);
        assert_eq!(cleared_summary.error_requests, 0);
        let cleared_page = usage_store
            .query(UsageRecordQuery {
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(cleared_page.total, 0);
        let soft_deleted_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM usage_records WHERE deleted_at IS NOT NULL")
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(soft_deleted_count, 2);
        let mut duplicate_after_cleanup = usage_record("usage-1", 30);
        duplicate_after_cleanup.created_at =
            (cleanup_cutoff + chrono::Duration::milliseconds(1)).to_rfc3339();
        usage_store.record(duplicate_after_cleanup).await.unwrap();
        let duplicate_page = usage_store
            .query(UsageRecordQuery {
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            duplicate_page.total, 0,
            "a tombstoned request id must not be revived"
        );

        let mut new_after_cleanup = usage_record("usage-3", 30);
        new_after_cleanup.created_at =
            (cleanup_cutoff + chrono::Duration::milliseconds(1)).to_rfc3339();
        usage_store.record(new_after_cleanup).await.unwrap();
        let restored = usage_store
            .query(UsageRecordQuery {
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(restored.total, 1);
        assert_eq!(restored.records[0].id, "usage-3");
        let restored_summary = usage_store.summary(15).await.unwrap();
        assert_eq!(restored_summary.total_requests, 1);
        assert_eq!(restored_summary.high_cache_requests, 1);

        let status = PricingStatus {
            available: true,
            source: "test".to_string(),
            source_url: "local".to_string(),
            model_count: 1,
            last_synced_at: Some(Utc::now().to_rfc3339()),
            last_error: None,
            models: vec![ModelPriceItem {
                model: "claude-sonnet-4-5".to_string(),
                pricing: ModelPricing {
                    input_cost_per_token: 0.000003,
                    output_cost_per_token: 0.000015,
                    cache_creation_input_token_cost: 0.00000375,
                    cache_read_input_token_cost: 0.0000003,
                },
                source: None,
            }],
        };
        store.save_pricing_status(&status).await.unwrap();
        let pricing = store.load_model_pricing().await.unwrap();
        assert!(pricing.contains_key("claude-sonnet-4-5"));
        let pricing_status = store.load_pricing_status().await.unwrap().unwrap();
        assert_eq!(pricing_status.source, "test");
        assert_eq!(pricing_status.source_url, "local");
        assert_eq!(pricing_status.last_synced_at, status.last_synced_at);
        assert_eq!(pricing_status.model_count, 1);

        let capabilities_status = ModelCapabilitiesStatus {
            available: true,
            source: "test".to_string(),
            model_count: 1,
            last_synced_at: Some(Utc::now().to_rfc3339()),
            last_error: None,
            models: vec![ModelCapabilityItem {
                model: "claude-sonnet-4-9".to_string(),
                display_name: "Claude Sonnet 4.9".to_string(),
                description: Some("future test model".to_string()),
                max_input_tokens: Some(1_000_000),
                max_output_tokens: Some(128_000),
                supports_prompt_caching: Some(true),
                supported_input_types: vec!["TEXT".to_string(), "IMAGE".to_string()],
                source: None,
            }],
            reasoning_fields: BTreeMap::from([(
                "claude-sonnet-4-9".to_string(),
                KiroReasoningFieldCapability {
                    path: KiroReasoningFieldPath::Reasoning,
                    efforts: vec!["low".to_string(), "high".to_string(), "max".to_string()],
                    default_effort: Some("high".to_string()),
                },
            )]),
            reasoning_capability_cohort_keys: vec![KiroModelCapabilityCohortKey {
                endpoint_family: "ide".to_string(),
                auth_method: "social".to_string(),
                provider: "builderid".to_string(),
                effective_auth_region: "us-east-1".to_string(),
                effective_api_region: "us-east-1".to_string(),
                subscription_class: "kiro pro".to_string(),
                supported_models: Vec::new(),
            }],
            reasoning_capability_cohort_complete: true,
            reasoning_capability_contract_version: 1,
            reasoning_invalid_models: vec!["claude-invalid-test".to_string()],
        };
        store
            .save_model_capabilities_status(&capabilities_status)
            .await
            .unwrap();
        let loaded_capabilities = store
            .load_model_capabilities_status()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded_capabilities.source, "test");
        assert_eq!(
            loaded_capabilities.last_synced_at,
            capabilities_status.last_synced_at
        );
        assert_eq!(loaded_capabilities.model_count, 1);
        assert_eq!(
            loaded_capabilities.reasoning_capability_cohort_keys,
            capabilities_status.reasoning_capability_cohort_keys
        );
        assert!(loaded_capabilities.reasoning_capability_cohort_complete);
        assert_eq!(loaded_capabilities.reasoning_capability_contract_version, 1);
        assert_eq!(
            loaded_capabilities.reasoning_invalid_models,
            capabilities_status.reasoning_invalid_models
        );
        assert_eq!(loaded_capabilities.models[0].model, "claude-sonnet-4-9");
        assert_eq!(
            loaded_capabilities
                .reasoning_fields
                .get("claude-sonnet-4-9")
                .map(|capability| capability.path),
            Some(KiroReasoningFieldPath::Reasoning)
        );
        assert_eq!(
            loaded_capabilities.models[0].max_input_tokens,
            Some(1_000_000)
        );
        assert_eq!(
            loaded_capabilities.models[0].supported_input_types,
            vec!["TEXT".to_string(), "IMAGE".to_string()]
        );

        let manual_capability = ModelCapabilityItem {
            model: "claude-opus-5-manual".to_string(),
            display_name: "Claude Opus 5 Manual".to_string(),
            description: None,
            max_input_tokens: Some(1_000_000),
            max_output_tokens: Some(128_000),
            supports_prompt_caching: Some(true),
            supported_input_types: vec!["TEXT".to_string()],
            source: Some(MANUAL_SOURCE.to_string()),
        };
        store
            .save_manual_model(
                &manual_capability,
                Some(ModelPricing {
                    input_cost_per_token: 0.00001,
                    output_cost_per_token: 0.00002,
                    cache_creation_input_token_cost: 0.0000125,
                    cache_read_input_token_cost: 0.000001,
                }),
                false,
            )
            .await
            .unwrap();
        store.save_pricing_status(&status).await.unwrap();
        store
            .save_model_capabilities_status(&capabilities_status)
            .await
            .unwrap();
        let loaded_pricing = store.load_pricing_status().await.unwrap().unwrap();
        assert!(
            loaded_pricing
                .models
                .iter()
                .any(|item| item.model == "claude-opus-5-manual"
                    && item.source.as_deref() == Some(MANUAL_PRICING_SOURCE))
        );
        let loaded_capabilities = store
            .load_model_capabilities_status()
            .await
            .unwrap()
            .unwrap();
        assert!(
            loaded_capabilities
                .models
                .iter()
                .any(|item| item.model == "claude-opus-5-manual"
                    && item.source.as_deref() == Some(MANUAL_SOURCE))
        );

        store
            .record_admin_audit_log(
                "test",
                "set_credential_priority",
                "credential",
                Some("7"),
                true,
                None,
                serde_json::json!({ "priority": 1 }),
            )
            .await
            .unwrap();
        let audit_page = store.query_admin_audit_logs(1, 20).await.unwrap();
        assert_eq!(audit_page.records.len(), 1);
        assert_eq!(audit_page.records[0].action, "set_credential_priority");
        assert_eq!(audit_page.records[0].object_id.as_deref(), Some("7"));

        store.save_credential_stats(&HashMap::new()).await.unwrap();
        assert!(
            store
                .load_credential_stats()
                .await
                .unwrap()
                .contains_key(&7),
            "空 stats 快照不应删除已有统计行"
        );
        store.delete_credential_stats_and_runtime(7).await.unwrap();
        assert!(store.load_credential_stats().await.unwrap().is_empty());
        assert!(
            store
                .load_credential_runtime_state()
                .await
                .unwrap()
                .is_empty()
        );

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_credential_upsert_rejects_soft_deleted_rows() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;
        let inserted = store
            .insert_credential(&KiroCredentials {
                email: Some("before-delete@example.com".to_string()),
                refresh_token: Some("refresh-before-delete".to_string()),
                auth_method: Some("social".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let credential_id = inserted.id.unwrap();
        store.soft_delete_credential(credential_id).await.unwrap();

        let mut stale = inserted;
        stale.email = Some("stale-writer@example.com".to_string());
        stale.priority = 99;
        let error = store.upsert_credential(&stale).await.unwrap_err();
        assert!(error.to_string().contains("已被删除"));

        let row = sqlx::query(
            r#"
            SELECT deleted_at IS NOT NULL AS is_deleted,
                   priority,
                   data->>'email' AS email
            FROM credentials
            WHERE id = $1
            "#,
        )
        .bind(credential_id as i64)
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert!(row.try_get::<bool, _>("is_deleted").unwrap());
        assert_eq!(row.try_get::<i32, _>("priority").unwrap(), 0);
        assert_eq!(
            row.try_get::<Option<String>, _>("email")
                .unwrap()
                .as_deref(),
            Some("before-delete@example.com")
        );
        assert!(store.load_credentials().await.unwrap().is_empty());

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_credential_revision_cas_preserves_concurrent_field_updates() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;
        let original = store
            .insert_credential(&KiroCredentials {
                access_token: Some("revision-access-old".to_string()),
                refresh_token: Some("revision-refresh-old".to_string()),
                auth_method: Some("social".to_string()),
                email: Some("revision-original@example.com".to_string()),
                priority: 1,
                ..Default::default()
            })
            .await
            .unwrap();
        let credential_id = original.id.unwrap();
        assert_eq!(original.storage_revision, 1);

        let mut priority_update = original.clone();
        priority_update.priority = 17;
        let mut email_update = original.clone();
        email_update.email = Some("revision-concurrent@example.com".to_string());
        let first_store = store.clone();
        let second_store = store.clone();
        let (priority_outcome, email_outcome) = tokio::join!(
            first_store.upsert_credential(&priority_update),
            second_store.upsert_credential(&email_update),
        );
        let priority_outcome = priority_outcome.unwrap();
        let email_outcome = email_outcome.unwrap();
        let mut retry = match (priority_outcome, email_outcome) {
            (
                CredentialUpsertCasOutcome::Applied(applied),
                CredentialUpsertCasOutcome::Conflict { current },
            ) => {
                assert_eq!(applied.priority, 17);
                assert_eq!(applied.storage_revision, 2);
                assert_eq!(current.storage_revision, 2);
                let mut retry = current;
                retry.email = Some("revision-concurrent@example.com".to_string());
                retry
            }
            (
                CredentialUpsertCasOutcome::Conflict { current },
                CredentialUpsertCasOutcome::Applied(applied),
            ) => {
                assert_eq!(
                    applied.email.as_deref(),
                    Some("revision-concurrent@example.com")
                );
                assert_eq!(applied.storage_revision, 2);
                assert_eq!(current.storage_revision, 2);
                let mut retry = current;
                retry.priority = 17;
                retry
            }
            _ => panic!("exactly one writer must apply for the same base revision"),
        };
        let retried = store.upsert_credential(&retry).await.unwrap();
        let CredentialUpsertCasOutcome::Applied(retried) = retried else {
            panic!("field-level retry against the authoritative revision must apply");
        };
        assert_eq!(retried.priority, 17);
        assert_eq!(
            retried.email.as_deref(),
            Some("revision-concurrent@example.com")
        );
        assert_eq!(retried.storage_revision, 3);

        retry = original.clone();
        retry.email = Some("revision-stale-overwrite@example.com".to_string());
        let stale = store.upsert_credential(&retry).await.unwrap();
        let CredentialUpsertCasOutcome::Conflict { current } = stale else {
            panic!("stale full-row credential update must conflict");
        };
        assert_eq!(current.priority, 17);
        assert_eq!(
            current.email.as_deref(),
            Some("revision-concurrent@example.com")
        );
        assert_eq!(current.storage_revision, 3);

        let refresh_context = CredentialRefreshExpectedContext::from_credentials(&current).unwrap();
        let refreshed = store
            .update_credential_refresh_fields_cas(
                credential_id,
                &refresh_context,
                &CredentialRefreshFieldsPatch {
                    access_token: Some("revision-access-new".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let CredentialRefreshFieldsCasOutcome::Applied(refreshed) = refreshed else {
            panic!("refresh fields must apply against the current auth context");
        };
        assert_eq!(refreshed.storage_revision, 4);

        let runtime = store
            .patch_credential_runtime_state(
                credential_id,
                Uuid::new_v4(),
                &CredentialRuntimeStatePatch {
                    credential_disabled: Some(true),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(runtime.credential_disabled);
        let authoritative = store
            .load_credentials()
            .await
            .unwrap()
            .into_iter()
            .find(|credential| credential.id == Some(credential_id))
            .unwrap();
        assert!(authoritative.disabled);
        assert_eq!(authoritative.storage_revision, 5);
        assert_eq!(authoritative.priority, 17);
        assert_eq!(
            authoritative.email.as_deref(),
            Some("revision-concurrent@example.com")
        );
        assert_eq!(
            authoritative.access_token.as_deref(),
            Some("revision-access-new")
        );

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_credential_revision_migration_upgrades_legacy_rows() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;
        let credential_id = store
            .insert_credential(&KiroCredentials {
                kiro_api_key: Some("ksk_credential_revision_migration".to_string()),
                auth_method: Some("api_key".to_string()),
                ..Default::default()
            })
            .await
            .unwrap()
            .id
            .unwrap();
        sqlx::query("ALTER TABLE credentials DROP COLUMN revision")
            .execute(store.pool())
            .await
            .unwrap();
        sqlx::query(
            "DELETE FROM schema_migrations WHERE version = 'credential-storage-revision-v1'",
        )
        .execute(store.pool())
        .await
        .unwrap();

        store.migrate_with_options(false).await.unwrap();
        let revision: i64 = sqlx::query_scalar("SELECT revision FROM credentials WHERE id = $1")
            .bind(credential_id as i64)
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(revision, 1);
        let loaded = store
            .load_credentials()
            .await
            .unwrap()
            .into_iter()
            .find(|credential| credential.id == Some(credential_id))
            .unwrap();
        assert_eq!(loaded.storage_revision, 1);

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_atomic_credential_insert_rolls_back_when_runtime_patch_fails() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;
        sqlx::query(
            r#"
            ALTER TABLE credential_runtime_state
            ADD CONSTRAINT test_atomic_insert_runtime_failure
            CHECK (warmup_remaining <> 99)
            "#,
        )
        .execute(store.pool())
        .await
        .unwrap();
        let credential = KiroCredentials {
            refresh_token: Some("atomic-insert-refresh".to_string()),
            auth_method: Some("social".to_string()),
            email: Some("atomic-insert@example.com".to_string()),
            ..Default::default()
        };
        let operation_id = Uuid::new_v4();
        let patch = CredentialRuntimeStatePatch {
            failure_count: Some(2),
            refresh_failure_count: Some(3),
            disabled_reason: CredentialRuntimeDisabledReasonPatch::Set("Manual".to_string()),
            warmup_remaining: Some(99),
            credential_disabled: Some(true),
            last_used_at: Some("2026-07-10T08:00:00Z".to_string()),
            ..Default::default()
        };
        assert!(
            store
                .insert_credential_with_runtime_patch(&credential, operation_id, &patch)
                .await
                .is_err()
        );
        let credential_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM credentials WHERE data->>'refreshToken' = 'atomic-insert-refresh'",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(credential_count, 0);
        let runtime_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*)::bigint FROM credential_runtime_state")
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(runtime_count, 0);
        let mutation_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM credential_runtime_mutations WHERE operation_id = $1",
        )
        .bind(operation_id.to_string())
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(mutation_count, 0);
        let stats_count: i64 = sqlx::query_scalar("SELECT COUNT(*)::bigint FROM credential_stats")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(stats_count, 0);

        sqlx::query(
            "ALTER TABLE credential_runtime_state DROP CONSTRAINT test_atomic_insert_runtime_failure",
        )
        .execute(store.pool())
        .await
        .unwrap();
        let mut valid_patch = patch;
        valid_patch.warmup_remaining = Some(4);
        let (inserted, runtime) = store
            .insert_credential_with_runtime_patch(&credential, operation_id, &valid_patch)
            .await
            .unwrap();
        assert_eq!(inserted.storage_revision, 1);
        assert!(inserted.disabled);
        assert_eq!(runtime.state.failure_count, 2);
        assert_eq!(runtime.state.refresh_failure_count, 3);
        assert_eq!(runtime.state.disabled_reason.as_deref(), Some("Manual"));
        assert_eq!(runtime.state.warmup_remaining, 4);
        assert_eq!(runtime.state.revision, 1);
        assert!(runtime.credential_disabled);

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_atomic_credential_update_rolls_back_with_runtime_patch() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;
        let (original, original_runtime) = store
            .insert_credential_with_runtime_patch(
                &KiroCredentials {
                    refresh_token: Some("atomic-update-refresh-old".to_string()),
                    access_token: Some("atomic-update-access-old".to_string()),
                    auth_method: Some("social".to_string()),
                    email: Some("atomic-update-old@example.com".to_string()),
                    ..Default::default()
                },
                Uuid::new_v4(),
                &CredentialRuntimeStatePatch {
                    failure_count: Some(8),
                    refresh_failure_count: Some(9),
                    disabled_reason: CredentialRuntimeDisabledReasonPatch::Set(
                        "Manual".to_string(),
                    ),
                    warmup_remaining: Some(5),
                    credential_disabled: Some(true),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(original.storage_revision, 1);
        assert_eq!(original_runtime.state.revision, 1);
        let credential_id = original.id.unwrap();

        sqlx::query(
            r#"
            ALTER TABLE credential_runtime_state
            ADD CONSTRAINT test_atomic_update_runtime_failure
            CHECK (warmup_remaining <> 77)
            "#,
        )
        .execute(store.pool())
        .await
        .unwrap();
        let mut updated = original.clone();
        updated.refresh_token = Some("atomic-update-refresh-new".to_string());
        updated.access_token = Some("atomic-update-access-new".to_string());
        updated.email = Some("atomic-update-new@example.com".to_string());
        let operation_id = Uuid::new_v4();
        let failing_patch = CredentialRuntimeStatePatch {
            failure_count: Some(0),
            refresh_failure_count: Some(0),
            disabled_reason: CredentialRuntimeDisabledReasonPatch::Clear,
            warmup_remaining: Some(77),
            credential_disabled: Some(false),
            ..Default::default()
        };
        assert!(
            store
                .update_credential_with_runtime_patch_cas(&updated, operation_id, &failing_patch,)
                .await
                .is_err()
        );
        let after_failure = store
            .load_credentials()
            .await
            .unwrap()
            .into_iter()
            .find(|credential| credential.id == Some(credential_id))
            .unwrap();
        assert_eq!(after_failure.storage_revision, 1);
        assert_eq!(
            after_failure.refresh_token.as_deref(),
            Some("atomic-update-refresh-old")
        );
        assert!(after_failure.disabled);
        let after_failure_runtime = store
            .load_credential_runtime_state()
            .await
            .unwrap()
            .remove(&credential_id)
            .unwrap();
        assert_eq!(after_failure_runtime.failure_count, 8);
        assert_eq!(after_failure_runtime.warmup_remaining, 5);
        assert_eq!(after_failure_runtime.revision, 1);

        sqlx::query(
            "ALTER TABLE credential_runtime_state DROP CONSTRAINT test_atomic_update_runtime_failure",
        )
        .execute(store.pool())
        .await
        .unwrap();
        let mut valid_patch = failing_patch;
        valid_patch.warmup_remaining = Some(0);
        let outcome = store
            .update_credential_with_runtime_patch_cas(&updated, operation_id, &valid_patch)
            .await
            .unwrap();
        let CredentialWithRuntimePatchCasOutcome::Applied {
            credential,
            runtime,
        } = outcome
        else {
            panic!("fresh credential/runtime revision must apply atomically");
        };
        assert_eq!(credential.storage_revision, 2);
        assert_eq!(
            credential.refresh_token.as_deref(),
            Some("atomic-update-refresh-new")
        );
        assert!(!credential.disabled);
        assert_eq!(runtime.state.failure_count, 0);
        assert_eq!(runtime.state.refresh_failure_count, 0);
        assert_eq!(runtime.state.disabled_reason, None);
        assert_eq!(runtime.state.warmup_remaining, 0);
        assert_eq!(runtime.state.revision, 2);
        assert!(!runtime.credential_disabled);

        let duplicate = store
            .update_credential_with_runtime_patch_cas(&updated, operation_id, &valid_patch)
            .await
            .unwrap();
        let CredentialWithRuntimePatchCasOutcome::Conflict { current } = duplicate else {
            panic!("a retry with the stale credential revision must reconcile as a conflict");
        };
        assert_eq!(current.storage_revision, 2);
        let runtime_after_duplicate = store
            .load_credential_runtime_state()
            .await
            .unwrap()
            .remove(&credential_id)
            .unwrap();
        assert_eq!(runtime_after_duplicate.revision, 2);
        let applied_mutation_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM credential_runtime_mutations WHERE operation_id = $1",
        )
        .bind(operation_id.to_string())
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(applied_mutation_count, 1);

        let stale_operation_id = Uuid::new_v4();
        let stale = store
            .update_credential_with_runtime_patch_cas(
                &original,
                stale_operation_id,
                &CredentialRuntimeStatePatch::default(),
            )
            .await
            .unwrap();
        let CredentialWithRuntimePatchCasOutcome::Conflict { current } = stale else {
            panic!("stale credential revision must not apply the runtime patch");
        };
        assert_eq!(current.storage_revision, 2);
        let stale_mutation_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM credential_runtime_mutations WHERE operation_id = $1",
        )
        .bind(stale_operation_id.to_string())
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(stale_mutation_count, 0);

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_refresh_field_cas_preserves_admin_changes_and_rejects_stale_hashes() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;
        let inserted = store
            .insert_credential(&KiroCredentials {
                access_token: Some("access-old".to_string()),
                refresh_token: Some("refresh-old".to_string()),
                profile_arn: Some("profile-old".to_string()),
                expires_at: Some("2026-07-10T00:00:00Z".to_string()),
                scopes: Some("scope-old".to_string()),
                auth_method: Some("social".to_string()),
                email: Some("owner@example.com".to_string()),
                priority: 2,
                proxy_url: Some("http://proxy-before.example:8080".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let credential_id = inserted.id.unwrap();
        let expected_old = CredentialRefreshExpectedContext::from_credentials(&inserted).unwrap();

        let mut admin_update = inserted.clone();
        admin_update.email = Some("admin-update@example.com".to_string());
        admin_update.priority = 17;
        admin_update.disabled = true;
        admin_update.max_concurrent_requests = Some(23);
        admin_update.proxy_url = Some("http://proxy-after.example:8080".to_string());
        let admin_update = store.upsert_credential(&admin_update).await.unwrap();
        let CredentialUpsertCasOutcome::Applied(admin_update) = admin_update else {
            panic!("fresh admin revision must apply");
        };
        assert_eq!(admin_update.storage_revision, 2);

        let applied = store
            .update_credential_refresh_fields_cas(
                credential_id,
                &expected_old,
                &CredentialRefreshFieldsPatch {
                    access_token: Some("access-new".to_string()),
                    refresh_token: Some("refresh-new".to_string()),
                    profile_arn: Some("profile-new".to_string()),
                    expires_at: Some("2026-07-11T00:00:00Z".to_string()),
                    scopes: Some("scope-new".to_string()),
                },
            )
            .await
            .unwrap();
        let CredentialRefreshFieldsCasOutcome::Applied(applied) = applied else {
            panic!("fresh refresh hash must apply");
        };
        assert_eq!(applied.access_token.as_deref(), Some("access-new"));
        assert_eq!(applied.refresh_token.as_deref(), Some("refresh-new"));
        assert_eq!(applied.profile_arn.as_deref(), Some("profile-new"));
        assert_eq!(applied.expires_at.as_deref(), Some("2026-07-11T00:00:00Z"));
        assert_eq!(applied.scopes.as_deref(), Some("scope-new"));
        assert_eq!(applied.email.as_deref(), Some("admin-update@example.com"));
        assert_eq!(applied.priority, 17);
        assert!(applied.disabled);
        assert_eq!(applied.max_concurrent_requests, Some(23));
        assert_eq!(
            applied.proxy_url.as_deref(),
            Some("http://proxy-after.example:8080")
        );

        let stored_refresh_hash: Option<String> =
            sqlx::query_scalar("SELECT refresh_token_hash FROM credentials WHERE id = $1")
                .bind(credential_id as i64)
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(
            stored_refresh_hash.as_deref(),
            Some(sha256_hex("refresh-new").as_str())
        );

        let stale = store
            .update_credential_refresh_fields_cas(
                credential_id,
                &expected_old,
                &CredentialRefreshFieldsPatch {
                    access_token: Some("access-stale".to_string()),
                    refresh_token: Some("refresh-stale".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let CredentialRefreshFieldsCasOutcome::Conflict {
            current: Some(current),
        } = stale
        else {
            panic!("stale refresh hash must return the authoritative credential");
        };
        assert_eq!(current.access_token.as_deref(), Some("access-new"));
        assert_eq!(current.refresh_token.as_deref(), Some("refresh-new"));
        assert_eq!(current.priority, 17);
        assert!(current.disabled);

        let expected_new = CredentialRefreshExpectedContext::from_credentials(&applied).unwrap();
        let mut auth_context_update = applied.clone();
        auth_context_update.auth_method = Some("external_idp".to_string());
        auth_context_update.provider = Some("EntraId".to_string());
        auth_context_update.client_id = Some("client-after".to_string());
        auth_context_update.client_secret = Some("secret-after".to_string());
        auth_context_update.token_endpoint =
            Some("https://login.example.test/oauth2/v2.0/token".to_string());
        auth_context_update.scopes = Some("scope-admin-change".to_string());
        let auth_context_update = store.upsert_credential(&auth_context_update).await.unwrap();
        let CredentialUpsertCasOutcome::Applied(auth_context_update) = auth_context_update else {
            panic!("fresh auth context revision must apply");
        };
        let auth_context_conflict = store
            .update_credential_refresh_fields_cas(
                credential_id,
                &expected_new,
                &CredentialRefreshFieldsPatch {
                    access_token: Some("access-from-old-auth-context".to_string()),
                    scopes: Some("scope-from-old-auth-context".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let CredentialRefreshFieldsCasOutcome::Conflict {
            current: Some(current),
        } = auth_context_conflict
        else {
            panic!("changed auth context must reject an in-flight refresh result");
        };
        assert_eq!(current.access_token.as_deref(), Some("access-new"));
        assert_eq!(current.auth_method.as_deref(), Some("external_idp"));
        assert_eq!(current.provider.as_deref(), Some("EntraId"));
        assert_eq!(current.client_id.as_deref(), Some("client-after"));
        assert_eq!(current.client_secret.as_deref(), Some("secret-after"));
        assert_eq!(current.scopes.as_deref(), Some("scope-admin-change"));

        let expected_after_auth_update =
            CredentialRefreshExpectedContext::from_credentials(&auth_context_update).unwrap();
        store.soft_delete_credential(credential_id).await.unwrap();
        let deleted = store
            .update_credential_refresh_fields_cas(
                credential_id,
                &expected_after_auth_update,
                &CredentialRefreshFieldsPatch {
                    access_token: Some("access-after-delete".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            deleted,
            CredentialRefreshFieldsCasOutcome::Conflict { current: None }
        ));
        let stored_access_token: Option<String> =
            sqlx::query_scalar("SELECT data->>'accessToken' FROM credentials WHERE id = $1")
                .bind(credential_id as i64)
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(stored_access_token.as_deref(), Some("access-new"));

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn postgres_refresh_field_cas_fences_non_rotating_refresh_by_access_token_for_five_rounds()
     {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        for round in 1..=5 {
            clean(&store).await;
            let old_access = format!("access-old-{round}");
            let shared_refresh = format!("refresh-non-rotating-{round}");
            let inserted = store
                .insert_credential(&KiroCredentials {
                    access_token: Some(old_access),
                    refresh_token: Some(shared_refresh.clone()),
                    expires_at: Some("2026-07-10T00:00:00Z".to_string()),
                    auth_method: Some("external_idp".to_string()),
                    email: Some("before@example.com".to_string()),
                    priority: 2,
                    ..Default::default()
                })
                .await
                .unwrap();
            let credential_id = inserted.id.unwrap();
            let expected = CredentialRefreshExpectedContext::from_credentials(&inserted).unwrap();

            let mut metadata_update = inserted.clone();
            metadata_update.email = Some(format!("admin-{round}@example.com"));
            metadata_update.priority = 20 + round;
            let CredentialUpsertCasOutcome::Applied(metadata_update) =
                store.upsert_credential(&metadata_update).await.unwrap()
            else {
                panic!("round {round}: metadata update must apply");
            };

            let first_token = format!("access-winner-a-{round}");
            let second_token = format!("access-winner-b-{round}");
            let first_patch = CredentialRefreshFieldsPatch {
                access_token: Some(first_token.clone()),
                expires_at: Some("2026-07-11T00:00:00Z".to_string()),
                ..Default::default()
            };
            let second_patch = CredentialRefreshFieldsPatch {
                access_token: Some(second_token.clone()),
                expires_at: Some("2026-07-11T00:00:01Z".to_string()),
                ..Default::default()
            };
            let (first, second) = tokio::join!(
                store.update_credential_refresh_fields_cas(credential_id, &expected, &first_patch,),
                store
                    .update_credential_refresh_fields_cas(credential_id, &expected, &second_patch,),
            );
            let mut applied = Vec::new();
            let mut conflicts = Vec::new();
            for outcome in [first.unwrap(), second.unwrap()] {
                match outcome {
                    CredentialRefreshFieldsCasOutcome::Applied(current) => applied.push(current),
                    CredentialRefreshFieldsCasOutcome::Conflict {
                        current: Some(current),
                    } => conflicts.push(current),
                    CredentialRefreshFieldsCasOutcome::Conflict { current: None } => {
                        panic!("round {round}: credential disappeared")
                    }
                }
            }
            assert_eq!(applied.len(), 1, "round {round}");
            assert_eq!(conflicts.len(), 1, "round {round}");
            let winner = &applied[0];
            assert!(
                matches!(
                    winner.access_token.as_deref(),
                    Some(value) if value == first_token || value == second_token
                ),
                "round {round}"
            );
            assert_eq!(
                conflicts[0].access_token, winner.access_token,
                "round {round}: loser must observe the authoritative winner"
            );
            assert_eq!(
                winner.refresh_token.as_deref(),
                Some(shared_refresh.as_str())
            );
            assert_eq!(
                winner.email, metadata_update.email,
                "round {round}: unrelated Admin metadata must survive"
            );
            assert_eq!(winner.priority, metadata_update.priority, "round {round}");
            assert_eq!(winner.storage_revision, 3, "round {round}");
        }

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_credential_hash_repair_backfills_legacy_rows_and_detects_collisions() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;
        let legacy_id = 501_i64;
        sqlx::query(
            r#"
            INSERT INTO credentials (
                id, priority, disabled, auth_kind,
                api_key_hash, refresh_token_hash, data
            )
            VALUES ($1, 4, false, 'oauth', NULL, NULL, $2)
            "#,
        )
        .bind(legacy_id)
        .bind(serde_json::json!({
            "refreshToken": "legacy-refresh-token",
            "authMethod": "builderId",
            "clientId": "legacy-client",
            "clientSecret": "legacy-secret",
            "scopes": "legacy-scope"
        }))
        .execute(store.pool())
        .await
        .unwrap();

        store.migrate_with_options(false).await.unwrap();
        let repaired =
            sqlx::query("SELECT auth_kind, refresh_token_hash FROM credentials WHERE id = $1")
                .bind(legacy_id)
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(repaired.try_get::<String, _>("auth_kind").unwrap(), "idc");
        assert_eq!(
            repaired
                .try_get::<Option<String>, _>("refresh_token_hash")
                .unwrap()
                .as_deref(),
            Some(sha256_hex("legacy-refresh-token").as_str())
        );

        let legacy = store
            .load_credentials()
            .await
            .unwrap()
            .into_iter()
            .find(|credential| credential.id == Some(legacy_id as u64))
            .unwrap();
        assert_eq!(legacy.auth_method.as_deref(), Some("idc"));
        let expected = CredentialRefreshExpectedContext::from_credentials(&legacy).unwrap();
        let applied = store
            .update_credential_refresh_fields_cas(
                legacy_id as u64,
                &expected,
                &CredentialRefreshFieldsPatch {
                    access_token: Some("legacy-access-new".to_string()),
                    refresh_token: Some("legacy-refresh-new".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let CredentialRefreshFieldsCasOutcome::Applied(applied) = applied else {
            panic!("canonical expected context must match a legacy authMethod alias");
        };
        assert_eq!(applied.access_token.as_deref(), Some("legacy-access-new"));
        assert_eq!(applied.refresh_token.as_deref(), Some("legacy-refresh-new"));

        for credential_id in [502_i64, 503_i64] {
            sqlx::query(
                r#"
                INSERT INTO credentials (
                    id, priority, disabled, auth_kind,
                    api_key_hash, refresh_token_hash, data
                )
                VALUES ($1, 0, false, 'oauth', NULL, NULL, $2)
                "#,
            )
            .bind(credential_id)
            .bind(serde_json::json!({
                "refreshToken": "duplicate-legacy-refresh-token",
                "authMethod": "social"
            }))
            .execute(store.pool())
            .await
            .unwrap();
        }
        let collision = store.migrate_with_options(false).await.unwrap_err();
        assert!(collision.to_string().contains("refreshToken 重复"));
        let hashes: Vec<Option<String>> = sqlx::query_scalar(
            "SELECT refresh_token_hash FROM credentials WHERE id IN (502, 503) ORDER BY id",
        )
        .fetch_all(store.pool())
        .await
        .unwrap();
        assert_eq!(hashes, vec![None, None]);

        sqlx::query("UPDATE credentials SET deleted_at = now() WHERE id = 503")
            .execute(store.pool())
            .await
            .unwrap();
        store.migrate_with_options(false).await.unwrap();
        let repaired_hash: Option<String> =
            sqlx::query_scalar("SELECT refresh_token_hash FROM credentials WHERE id = 502")
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(
            repaired_hash.as_deref(),
            Some(sha256_hex("duplicate-legacy-refresh-token").as_str())
        );

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_stats_delta_batches_are_exactly_once_and_payload_bound() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;
        let first_id = store
            .insert_credential(&KiroCredentials {
                kiro_api_key: Some("ksk_stats_exactly_once_first".to_string()),
                auth_method: Some("api_key".to_string()),
                ..Default::default()
            })
            .await
            .unwrap()
            .id
            .unwrap();
        let second_id = store
            .insert_credential(&KiroCredentials {
                kiro_api_key: Some("ksk_stats_exactly_once_second".to_string()),
                auth_method: Some("api_key".to_string()),
                ..Default::default()
            })
            .await
            .unwrap()
            .id
            .unwrap();
        let deltas = HashMap::from([
            (
                first_id,
                CredentialStatsDeltaRow {
                    success_delta: 2,
                    selection_delta: 3,
                    last_used_at: Some("2026-07-10T03:00:00Z".to_string()),
                },
            ),
            (
                second_id,
                CredentialStatsDeltaRow {
                    success_delta: 5,
                    selection_delta: 7,
                    last_used_at: Some("2026-07-10T04:00:00Z".to_string()),
                },
            ),
        ]);
        let operation_id = Uuid::new_v4();
        let first_store = store.clone();
        let second_store = store.clone();
        let (first, duplicate) = tokio::join!(
            first_store.apply_credential_stats_deltas(operation_id, &deltas),
            second_store.apply_credential_stats_deltas(operation_id, &deltas),
        );
        first.unwrap();
        duplicate.unwrap();
        store
            .apply_credential_stats_deltas(operation_id, &deltas)
            .await
            .unwrap();

        let stats = store.load_credential_stats().await.unwrap();
        assert_eq!(stats.get(&first_id).unwrap().success_count, 2);
        assert_eq!(stats.get(&first_id).unwrap().selection_count, 3);
        assert_eq!(
            stats.get(&first_id).unwrap().last_used_at.as_deref(),
            Some("2026-07-10T03:00:00Z")
        );
        assert_eq!(stats.get(&second_id).unwrap().success_count, 5);
        assert_eq!(stats.get(&second_id).unwrap().selection_count, 7);

        let different_payload = HashMap::from([(
            first_id,
            CredentialStatsDeltaRow {
                success_delta: 100,
                ..Default::default()
            },
        )]);
        let payload_error = store
            .apply_credential_stats_deltas(operation_id, &different_payload)
            .await
            .unwrap_err();
        assert!(payload_error.to_string().contains("不同 payload"));
        assert_eq!(
            store
                .load_credential_stats()
                .await
                .unwrap()
                .get(&first_id)
                .unwrap()
                .success_count,
            2
        );
        let ledger_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM credential_stats_delta_batches WHERE operation_id = $1",
        )
        .bind(operation_id.to_string())
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(ledger_count, 1);

        sqlx::query(
            "UPDATE credential_stats_delta_batches SET created_at = now() - interval '2 days' WHERE operation_id = $1",
        )
        .bind(operation_id.to_string())
        .execute(store.pool())
        .await
        .unwrap();
        assert_eq!(
            store
                .cleanup_credential_runtime_mutations(std::time::Duration::from_secs(86_400), 1)
                .await
                .unwrap(),
            1
        );
        let ledger_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM credential_stats_delta_batches WHERE operation_id = $1",
        )
        .bind(operation_id.to_string())
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(ledger_count, 0);

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_stats_delta_batch_rolls_back_all_chunks() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;
        sqlx::query(
            r#"
            INSERT INTO credentials (
                id, priority, disabled, auth_kind,
                api_key_hash, refresh_token_hash, data
            )
            SELECT id,
                   0,
                   false,
                   'api_key',
                   NULL,
                   NULL,
                   jsonb_build_object(
                       'kiroApiKey', 'ksk_stats_chunk_' || id::text,
                       'authMethod', 'api_key'
                   )
            FROM generate_series(1, 1001) AS id
            "#,
        )
        .execute(store.pool())
        .await
        .unwrap();
        sqlx::query(
            r#"
            ALTER TABLE credential_stats
            ADD CONSTRAINT test_stats_second_chunk_failure
            CHECK (credential_id <> 1001)
            "#,
        )
        .execute(store.pool())
        .await
        .unwrap();
        let deltas = (1_u64..=1001)
            .map(|credential_id| {
                (
                    credential_id,
                    CredentialStatsDeltaRow {
                        success_delta: 1,
                        selection_delta: 1,
                        ..Default::default()
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        let operation_id = Uuid::new_v4();
        assert!(
            store
                .apply_credential_stats_deltas(operation_id, &deltas)
                .await
                .is_err()
        );
        let stats_count: i64 = sqlx::query_scalar("SELECT COUNT(*)::bigint FROM credential_stats")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(stats_count, 0, "earlier chunks must roll back");
        let ledger_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM credential_stats_delta_batches WHERE operation_id = $1",
        )
        .bind(operation_id.to_string())
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(
            ledger_count, 0,
            "failed batches must not reserve operation_id"
        );

        sqlx::query("ALTER TABLE credential_stats DROP CONSTRAINT test_stats_second_chunk_failure")
            .execute(store.pool())
            .await
            .unwrap();
        store
            .apply_credential_stats_deltas(operation_id, &deltas)
            .await
            .unwrap();
        let totals = sqlx::query(
            r#"
            SELECT COUNT(*)::bigint AS credential_count,
                   SUM(success_count)::bigint AS success_count,
                   SUM(selection_count)::bigint AS selection_count
            FROM credential_stats
            "#,
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(totals.try_get::<i64, _>("credential_count").unwrap(), 1001);
        assert_eq!(totals.try_get::<i64, _>("success_count").unwrap(), 1001);
        assert_eq!(totals.try_get::<i64, _>("selection_count").unwrap(), 1001);

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_stats_delta_batch_filters_soft_delete_races() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;
        let credential_id = store
            .insert_credential(&KiroCredentials {
                kiro_api_key: Some("ksk_stats_soft_delete_race".to_string()),
                auth_method: Some("api_key".to_string()),
                ..Default::default()
            })
            .await
            .unwrap()
            .id
            .unwrap();
        let mut delete_tx = store.pool().begin().await.unwrap();
        sqlx::query("UPDATE credentials SET deleted_at = now(), updated_at = now() WHERE id = $1")
            .bind(credential_id as i64)
            .execute(&mut *delete_tx)
            .await
            .unwrap();

        let operation_id = Uuid::new_v4();
        let deltas = HashMap::from([(
            credential_id,
            CredentialStatsDeltaRow {
                success_delta: 9,
                selection_delta: 11,
                last_used_at: Some("2026-07-10T05:00:00Z".to_string()),
            },
        )]);
        let task_store = store.clone();
        let task_deltas = deltas.clone();
        let mut stats_task = tokio::spawn(async move {
            task_store
                .apply_credential_stats_deltas(operation_id, &task_deltas)
                .await
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut stats_task)
                .await
                .is_err(),
            "stats batch should wait for the credential row lock"
        );
        delete_tx.commit().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), stats_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        store
            .apply_credential_stats_deltas(operation_id, &deltas)
            .await
            .unwrap();

        let stats_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM credential_stats WHERE credential_id = $1",
        )
        .bind(credential_id as i64)
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(stats_count, 0);
        let ledger = sqlx::query(
            r#"
            SELECT input_credential_count, applied_credential_count
            FROM credential_stats_delta_batches
            WHERE operation_id = $1
            "#,
        )
        .bind(operation_id.to_string())
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(
            ledger.try_get::<i32, _>("input_credential_count").unwrap(),
            1
        );
        assert_eq!(
            ledger
                .try_get::<i32, _>("applied_credential_count")
                .unwrap(),
            0
        );

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_last_used_at_compares_rfc3339_instants() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;
        let credential_id = store
            .insert_credential(&KiroCredentials {
                kiro_api_key: Some("ksk_last_used_rfc3339_order".to_string()),
                auth_method: Some("api_key".to_string()),
                ..Default::default()
            })
            .await
            .unwrap()
            .id
            .unwrap();

        for last_used_at in [
            "2026-07-10T04:00:00Z",
            "2026-07-10T05:30:00+02:00",
            "2026-07-10T04:30:00+00:00",
            "2026-07-10T00:45:00-04:00",
        ] {
            store
                .apply_credential_stats_deltas(
                    Uuid::new_v4(),
                    &HashMap::from([(
                        credential_id,
                        CredentialStatsDeltaRow {
                            last_used_at: Some(last_used_at.to_string()),
                            ..Default::default()
                        },
                    )]),
                )
                .await
                .unwrap();
        }
        let last_used_at: Option<String> = sqlx::query_scalar(
            "SELECT last_used_at FROM credential_stats WHERE credential_id = $1",
        )
        .bind(credential_id as i64)
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(
            last_used_at.as_deref(),
            Some("2026-07-10T00:45:00-04:00"),
            "offset timestamps must be ordered by instant rather than text"
        );

        store
            .record_credential_api_failure(
                credential_id,
                Uuid::new_v4(),
                "2026-07-10T06:00:00+00:00",
                10,
            )
            .await
            .unwrap();
        store
            .record_credential_refresh_failure(
                credential_id,
                Uuid::new_v4(),
                "2026-07-10T07:30:00+02:00",
                10,
            )
            .await
            .unwrap();
        let last_used_at: Option<String> = sqlx::query_scalar(
            "SELECT last_used_at FROM credential_stats WHERE credential_id = $1",
        )
        .bind(credential_id as i64)
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(last_used_at.as_deref(), Some("2026-07-10T06:00:00+00:00"));

        let invalid_operation_id = Uuid::new_v4();
        let error = store
            .apply_credential_stats_deltas(
                invalid_operation_id,
                &HashMap::from([(
                    credential_id,
                    CredentialStatsDeltaRow {
                        last_used_at: Some("not-a-timestamp".to_string()),
                        ..Default::default()
                    },
                )]),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("有效 RFC3339"));
        let invalid_ledger_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM credential_stats_delta_batches WHERE operation_id = $1",
        )
        .bind(invalid_operation_id.to_string())
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(invalid_ledger_count, 0);

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_runtime_mutations_are_idempotent_and_revisioned() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;
        let credential_id = store
            .insert_credential(&KiroCredentials {
                kiro_api_key: Some("ksk_runtime_mutation_revision".to_string()),
                auth_method: Some("api_key".to_string()),
                ..Default::default()
            })
            .await
            .unwrap()
            .id
            .unwrap();

        let failure_operation_id = Uuid::new_v4();
        let first_store = store.clone();
        let second_store = store.clone();
        let (failure, duplicate) = tokio::join!(
            first_store.record_credential_api_failure(
                credential_id,
                failure_operation_id,
                "2026-07-10T00:00:00Z",
                3,
            ),
            second_store.record_credential_api_failure(
                credential_id,
                failure_operation_id,
                "2026-07-10T00:00:00Z",
                3,
            ),
        );
        let failure = failure.unwrap();
        let duplicate = duplicate.unwrap();
        assert_eq!(failure.failure_count, 1);
        assert_eq!(failure.revision, 1);
        assert_eq!(duplicate.failure_count, 1);
        assert_eq!(duplicate.revision, 1);

        let refresh_failure = store
            .record_credential_refresh_failure(
                credential_id,
                Uuid::new_v4(),
                "2026-07-10T00:00:01Z",
                3,
            )
            .await
            .unwrap();
        assert_eq!(refresh_failure.failure_count, 1);
        assert_eq!(refresh_failure.refresh_failure_count, 1);
        assert_eq!(refresh_failure.revision, 2);

        let success = store
            .record_credential_success(credential_id, Uuid::new_v4())
            .await
            .unwrap();
        assert_eq!(success.failure_count, 0);
        assert_eq!(success.refresh_failure_count, 0);
        assert_eq!(success.revision, 3);

        let late_duplicate = store
            .record_credential_api_failure(
                credential_id,
                failure_operation_id,
                "2026-07-10T00:00:00Z",
                3,
            )
            .await
            .unwrap();
        assert_eq!(late_duplicate.failure_count, 0);
        assert_eq!(late_duplicate.revision, 3);

        let mutation_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM credential_runtime_mutations WHERE credential_id = $1",
        )
        .bind(credential_id as i64)
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(mutation_count, 3);

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_runtime_generation_fences_pre_reset_mutations() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;
        let credential_id = store
            .insert_credential(&KiroCredentials {
                kiro_api_key: Some("ksk_runtime_generation_fence".to_string()),
                auth_method: Some("api_key".to_string()),
                ..Default::default()
            })
            .await
            .unwrap()
            .id
            .unwrap();

        let reset_operation_id = Uuid::new_v4();
        let reset_patch = CredentialRuntimeStatePatch {
            failure_count: Some(0),
            refresh_failure_count: Some(0),
            disabled_reason: CredentialRuntimeDisabledReasonPatch::Clear,
            warmup_remaining: Some(0),
            credential_disabled: Some(false),
            expected_generation: Some(0),
            advance_generation: true,
            ..Default::default()
        };
        let reset = store
            .patch_credential_runtime_state(credential_id, reset_operation_id, &reset_patch)
            .await
            .unwrap();
        assert!(reset.applied);
        assert_eq!(reset.state.generation, 1);
        assert_eq!(reset.state.revision, 1);
        assert!(!reset.credential_disabled);

        let duplicate_reset = store
            .patch_credential_runtime_state(credential_id, reset_operation_id, &reset_patch)
            .await
            .unwrap();
        assert!(duplicate_reset.applied);
        assert_eq!(duplicate_reset, reset);

        let stale_failure_operation_id = Uuid::new_v4();
        let stale_failure = store
            .record_credential_api_failure_at_generation(
                credential_id,
                stale_failure_operation_id,
                0,
                "2026-07-10T00:00:01Z",
                1,
            )
            .await
            .unwrap();
        assert!(!stale_failure.applied);
        assert_eq!(stale_failure.state, reset.state);

        let stale_refresh_operation_id = Uuid::new_v4();
        let stale_refresh = store
            .record_credential_refresh_failure_at_generation(
                credential_id,
                stale_refresh_operation_id,
                0,
                "2026-07-10T00:00:02Z",
                1,
            )
            .await
            .unwrap();
        assert!(!stale_refresh.applied);
        assert_eq!(stale_refresh.state, reset.state);

        let stale_disable_operation_id = Uuid::new_v4();
        let stale_disable = store
            .mark_credential_disabled_at_generation(
                credential_id,
                stale_disable_operation_id,
                0,
                "InvalidRefreshToken",
                CredentialRuntimeFailureCounts {
                    failure_count: Some(1),
                    refresh_failure_count: None,
                },
                "2026-07-10T00:00:03Z",
            )
            .await
            .unwrap();
        assert!(!stale_disable.applied);
        assert_eq!(stale_disable.state, reset.state);
        assert!(!stale_disable.credential_disabled);

        let stale_ledger_count: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*)::bigint
            FROM credential_runtime_mutations
            WHERE operation_id = ANY($1)
            "#,
        )
        .bind(vec![
            stale_failure_operation_id.to_string(),
            stale_refresh_operation_id.to_string(),
            stale_disable_operation_id.to_string(),
        ])
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(stale_ledger_count, 0);
        let stale_stats_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM credential_stats WHERE credential_id = $1",
        )
        .bind(credential_id as i64)
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(stale_stats_count, 0);
        let credential_disabled: bool =
            sqlx::query_scalar("SELECT disabled FROM credentials WHERE id = $1")
                .bind(credential_id as i64)
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert!(!credential_disabled);

        let current_failure = store
            .record_credential_api_failure_at_generation(
                credential_id,
                Uuid::new_v4(),
                1,
                "2026-07-10T00:00:04Z",
                1,
            )
            .await
            .unwrap();
        assert!(current_failure.applied);
        assert_eq!(current_failure.state.generation, 1);
        assert_eq!(current_failure.state.failure_count, 1);
        assert_eq!(current_failure.state.revision, 2);
        assert!(current_failure.credential_disabled);

        let healed = store
            .heal_credential_api_failures(credential_id)
            .await
            .unwrap()
            .expect("threshold failure should be healed");
        assert_eq!(healed.generation, 2);
        assert_eq!(healed.failure_count, 0);
        assert_eq!(healed.disabled_reason, None);
        assert_eq!(healed.revision, 3);

        let pre_heal_operation_id = Uuid::new_v4();
        let pre_heal_failure = store
            .record_credential_api_failure_at_generation(
                credential_id,
                pre_heal_operation_id,
                1,
                "2026-07-10T00:00:05Z",
                1,
            )
            .await
            .unwrap();
        assert!(!pre_heal_failure.applied);
        assert_eq!(pre_heal_failure.state, healed);
        let pre_heal_ledger_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM credential_runtime_mutations WHERE operation_id = $1",
        )
        .bind(pre_heal_operation_id.to_string())
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(pre_heal_ledger_count, 0);

        let current_refresh = store
            .record_credential_refresh_failure_at_generation(
                credential_id,
                Uuid::new_v4(),
                2,
                "2026-07-10T00:00:06Z",
                3,
            )
            .await
            .unwrap();
        assert!(current_refresh.applied);
        assert_eq!(current_refresh.state.generation, 2);
        assert_eq!(current_refresh.state.refresh_failure_count, 1);
        assert_eq!(current_refresh.state.revision, 4);

        let current_success = store
            .record_credential_success_at_generation_with_count(credential_id, Uuid::new_v4(), 2, 1)
            .await
            .unwrap();
        assert!(current_success.applied);
        assert_eq!(current_success.state.generation, 2);
        assert_eq!(current_success.state.failure_count, 0);
        assert_eq!(current_success.state.refresh_failure_count, 0);
        assert_eq!(current_success.state.revision, 5);

        let future_operation_id = Uuid::new_v4();
        let future_error = store
            .record_credential_success_at_generation_with_count(
                credential_id,
                future_operation_id,
                3,
                1,
            )
            .await
            .unwrap_err();
        assert!(future_error.to_string().contains("generation 超前"));
        let future_ledger_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM credential_runtime_mutations WHERE operation_id = $1",
        )
        .bind(future_operation_id.to_string())
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(future_ledger_count, 0);

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_disable_mutations_are_idempotent_and_preserve_unspecified_counts() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;
        let credential_id = store
            .insert_credential(&KiroCredentials {
                kiro_api_key: Some("ksk_disable_mutation_idempotency".to_string()),
                auth_method: Some("api_key".to_string()),
                ..Default::default()
            })
            .await
            .unwrap()
            .id
            .unwrap();
        let initial = store
            .save_credential_runtime_state_for(
                credential_id,
                &CredentialRuntimeStateRow {
                    failure_count: 2,
                    refresh_failure_count: 3,
                    disabled_reason: None,
                    warmup_remaining: 0,
                    revision: 0,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let CredentialRuntimeStateCasOutcome::Applied(initial) = initial else {
            panic!("initial state must be applied");
        };
        assert_eq!(initial.revision, 1);

        let duplicate_operation_id = Uuid::new_v4();
        let first_store = store.clone();
        let second_store = store.clone();
        let (first, duplicate) = tokio::join!(
            first_store.mark_credential_disabled(
                credential_id,
                duplicate_operation_id,
                "QuotaExceeded",
                Some(7),
                None,
                "2026-07-10T00:00:00Z",
            ),
            second_store.mark_credential_disabled(
                credential_id,
                duplicate_operation_id,
                "QuotaExceeded",
                Some(7),
                None,
                "2026-07-10T00:00:00Z",
            ),
        );
        let first = first.unwrap();
        let duplicate = duplicate.unwrap();
        assert_eq!(first, duplicate);
        assert!(first.credential_disabled);
        let first = first.state;
        assert_eq!(first.failure_count, 7);
        assert_eq!(first.refresh_failure_count, 3);
        assert_eq!(first.disabled_reason.as_deref(), Some("QuotaExceeded"));
        assert_eq!(first.revision, 2);

        let refresh_operation_id = Uuid::new_v4();
        let failure_operation_id = Uuid::new_v4();
        let first_store = store.clone();
        let second_store = store.clone();
        let (refresh_update, failure_update) = tokio::join!(
            first_store.mark_credential_disabled(
                credential_id,
                refresh_operation_id,
                "AccountSuspended",
                None,
                Some(11),
                "2026-07-10T00:00:01Z",
            ),
            second_store.mark_credential_disabled(
                credential_id,
                failure_operation_id,
                "InvalidRefreshToken",
                Some(13),
                None,
                "2026-07-10T00:00:02Z",
            ),
        );
        refresh_update.unwrap();
        failure_update.unwrap();

        let stored = store
            .load_credential_runtime_state()
            .await
            .unwrap()
            .remove(&credential_id)
            .unwrap();
        assert_eq!(stored.failure_count, 13);
        assert_eq!(stored.refresh_failure_count, 11);
        assert!(matches!(
            stored.disabled_reason.as_deref(),
            Some("AccountSuspended" | "InvalidRefreshToken")
        ));
        assert_eq!(stored.revision, 4);

        let late_duplicate = store
            .mark_credential_disabled(
                credential_id,
                duplicate_operation_id,
                "QuotaExceeded",
                Some(7),
                None,
                "1900-01-01T00:00:00Z",
            )
            .await
            .unwrap();
        assert_eq!(late_duplicate.state, stored);
        assert!(late_duplicate.credential_disabled);
        let last_used_at: Option<String> = sqlx::query_scalar(
            "SELECT last_used_at FROM credential_stats WHERE credential_id = $1",
        )
        .bind(credential_id as i64)
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_ne!(last_used_at.as_deref(), Some("1900-01-01T00:00:00Z"));

        let credential_row = sqlx::query(
            "SELECT disabled, data->>'disabled' AS data_disabled FROM credentials WHERE id = $1",
        )
        .bind(credential_id as i64)
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert!(credential_row.try_get::<bool, _>("disabled").unwrap());
        assert_eq!(
            credential_row
                .try_get::<Option<String>, _>("data_disabled")
                .unwrap()
                .as_deref(),
            Some("true")
        );
        let mutation_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM credential_runtime_mutations WHERE credential_id = $1",
        )
        .bind(credential_id as i64)
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(mutation_count, 3);

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_runtime_patches_are_field_level_and_idempotent() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;
        let credential_id = store
            .insert_credential(&KiroCredentials {
                kiro_api_key: Some("ksk_runtime_patch_idempotency".to_string()),
                auth_method: Some("api_key".to_string()),
                ..Default::default()
            })
            .await
            .unwrap()
            .id
            .unwrap();
        let initial = store
            .save_credential_runtime_state_for(
                credential_id,
                &CredentialRuntimeStateRow {
                    failure_count: 2,
                    refresh_failure_count: 3,
                    disabled_reason: Some("Initial".to_string()),
                    warmup_remaining: 4,
                    revision: 0,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let CredentialRuntimeStateCasOutcome::Applied(initial) = initial else {
            panic!("initial state must be applied");
        };
        assert_eq!(initial.revision, 1);

        let failure_patch = CredentialRuntimeStatePatch {
            failure_count: Some(7),
            disabled_reason: CredentialRuntimeDisabledReasonPatch::Set("QuotaExceeded".to_string()),
            credential_disabled: Some(true),
            last_used_at: Some("2026-07-10T01:00:00Z".to_string()),
            ..Default::default()
        };
        let refresh_patch = CredentialRuntimeStatePatch {
            refresh_failure_count: Some(11),
            warmup_remaining: Some(9),
            ..Default::default()
        };
        let first_store = store.clone();
        let second_store = store.clone();
        let (failure_result, refresh_result) = tokio::join!(
            first_store.patch_credential_runtime_state(
                credential_id,
                Uuid::new_v4(),
                &failure_patch,
            ),
            second_store.patch_credential_runtime_state(
                credential_id,
                Uuid::new_v4(),
                &refresh_patch,
            ),
        );
        let failure_result = failure_result.unwrap();
        let refresh_result = refresh_result.unwrap();
        let mut applied_revisions = [failure_result.state.revision, refresh_result.state.revision];
        applied_revisions.sort_unstable();
        assert_eq!(applied_revisions, [2, 3]);

        let stored = store
            .load_credential_runtime_state()
            .await
            .unwrap()
            .remove(&credential_id)
            .unwrap();
        assert_eq!(stored.failure_count, 7);
        assert_eq!(stored.refresh_failure_count, 11);
        assert_eq!(stored.disabled_reason.as_deref(), Some("QuotaExceeded"));
        assert_eq!(stored.warmup_remaining, 9);
        assert_eq!(stored.revision, 3);

        let duplicate_operation_id = Uuid::new_v4();
        let clear_patch = CredentialRuntimeStatePatch {
            failure_count: Some(13),
            disabled_reason: CredentialRuntimeDisabledReasonPatch::Clear,
            credential_disabled: Some(false),
            last_used_at: Some("2026-07-10T02:00:00Z".to_string()),
            ..Default::default()
        };
        let first_store = store.clone();
        let second_store = store.clone();
        let (first, duplicate) = tokio::join!(
            first_store.patch_credential_runtime_state(
                credential_id,
                duplicate_operation_id,
                &clear_patch,
            ),
            second_store.patch_credential_runtime_state(
                credential_id,
                duplicate_operation_id,
                &clear_patch,
            ),
        );
        let first = first.unwrap();
        let duplicate = duplicate.unwrap();
        assert_eq!(first, duplicate);
        assert_eq!(first.state.failure_count, 13);
        assert_eq!(first.state.refresh_failure_count, 11);
        assert_eq!(first.state.disabled_reason, None);
        assert_eq!(first.state.warmup_remaining, 9);
        assert_eq!(first.state.revision, 4);
        assert!(!first.credential_disabled);

        let later = store
            .patch_credential_runtime_state(
                credential_id,
                Uuid::new_v4(),
                &CredentialRuntimeStatePatch {
                    disabled_reason: CredentialRuntimeDisabledReasonPatch::Set(
                        "LaterDisable".to_string(),
                    ),
                    warmup_remaining: Some(21),
                    credential_disabled: Some(true),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(later.state.revision, 5);
        assert!(later.credential_disabled);

        let late_duplicate = store
            .patch_credential_runtime_state(credential_id, duplicate_operation_id, &clear_patch)
            .await
            .unwrap();
        assert_eq!(late_duplicate, later);
        assert_eq!(late_duplicate.state.failure_count, 13);
        assert_eq!(late_duplicate.state.refresh_failure_count, 11);
        assert_eq!(late_duplicate.state.warmup_remaining, 21);
        assert_eq!(
            late_duplicate.state.disabled_reason.as_deref(),
            Some("LaterDisable")
        );
        assert!(late_duplicate.credential_disabled);

        let credential_row = sqlx::query(
            "SELECT disabled, data->>'disabled' AS data_disabled FROM credentials WHERE id = $1",
        )
        .bind(credential_id as i64)
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert!(credential_row.try_get::<bool, _>("disabled").unwrap());
        assert_eq!(
            credential_row
                .try_get::<Option<String>, _>("data_disabled")
                .unwrap()
                .as_deref(),
            Some("true")
        );
        let last_used_at: Option<String> = sqlx::query_scalar(
            "SELECT last_used_at FROM credential_stats WHERE credential_id = $1",
        )
        .bind(credential_id as i64)
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(last_used_at.as_deref(), Some("2026-07-10T02:00:00Z"));
        let mutation_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM credential_runtime_mutations WHERE credential_id = $1",
        )
        .bind(credential_id as i64)
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(mutation_count, 4);

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_runtime_mutations_and_snapshots_reject_soft_delete_races() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;
        let mutation_credential_id = store
            .insert_credential(&KiroCredentials {
                kiro_api_key: Some("ksk_runtime_patch_soft_delete".to_string()),
                auth_method: Some("api_key".to_string()),
                ..Default::default()
            })
            .await
            .unwrap()
            .id
            .unwrap();

        let mut delete_tx = store.pool().begin().await.unwrap();
        sqlx::query("UPDATE credentials SET deleted_at = now(), updated_at = now() WHERE id = $1")
            .bind(mutation_credential_id as i64)
            .execute(&mut *delete_tx)
            .await
            .unwrap();
        let patch_store = store.clone();
        let mut patch_task = tokio::spawn(async move {
            patch_store
                .patch_credential_runtime_state(
                    mutation_credential_id,
                    Uuid::new_v4(),
                    &CredentialRuntimeStatePatch {
                        failure_count: Some(5),
                        ..Default::default()
                    },
                )
                .await
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut patch_task)
                .await
                .is_err(),
            "runtime patch should wait for the credential row lock"
        );
        delete_tx.commit().await.unwrap();
        let patch_error = tokio::time::timeout(std::time::Duration::from_secs(5), patch_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(patch_error.to_string().contains("不存在或已删除"));

        let snapshot_credential_id = store
            .insert_credential(&KiroCredentials {
                kiro_api_key: Some("ksk_runtime_snapshot_soft_delete".to_string()),
                auth_method: Some("api_key".to_string()),
                ..Default::default()
            })
            .await
            .unwrap()
            .id
            .unwrap();
        let mut delete_tx = store.pool().begin().await.unwrap();
        sqlx::query("UPDATE credentials SET deleted_at = now(), updated_at = now() WHERE id = $1")
            .bind(snapshot_credential_id as i64)
            .execute(&mut *delete_tx)
            .await
            .unwrap();
        let snapshot_store = store.clone();
        let mut snapshot_task = tokio::spawn(async move {
            snapshot_store
                .save_credential_runtime_state_snapshot(
                    snapshot_credential_id,
                    &CredentialRuntimeStateSnapshot {
                        state: CredentialRuntimeStateRow {
                            failure_count: 8,
                            ..Default::default()
                        },
                        expected_revision: 0,
                    },
                )
                .await
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut snapshot_task,)
                .await
                .is_err(),
            "runtime snapshot should wait for the credential row lock"
        );
        delete_tx.commit().await.unwrap();
        let snapshot_error = tokio::time::timeout(std::time::Duration::from_secs(5), snapshot_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(snapshot_error.to_string().contains("不存在或已删除"));

        let runtime_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM credential_runtime_state WHERE credential_id IN ($1, $2)",
        )
        .bind(mutation_credential_id as i64)
        .bind(snapshot_credential_id as i64)
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(runtime_count, 0);
        let mutation_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM credential_runtime_mutations WHERE credential_id IN ($1, $2)",
        )
        .bind(mutation_credential_id as i64)
        .bind(snapshot_credential_id as i64)
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(mutation_count, 0);

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_runtime_snapshot_cas_rejects_stale_revision_without_writing() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;
        let credential_id = store
            .insert_credential(&KiroCredentials {
                kiro_api_key: Some("ksk_runtime_snapshot_stale".to_string()),
                auth_method: Some("api_key".to_string()),
                ..Default::default()
            })
            .await
            .unwrap()
            .id
            .unwrap();
        let initial = CredentialRuntimeStateSnapshot {
            state: CredentialRuntimeStateRow {
                failure_count: 1,
                refresh_failure_count: 0,
                disabled_reason: None,
                warmup_remaining: 4,
                revision: 0,
                ..Default::default()
            },
            expected_revision: 0,
        };
        let first = store
            .save_credential_runtime_state_snapshot(credential_id, &initial)
            .await
            .unwrap();
        let CredentialRuntimeStateCasOutcome::Applied(first) = first else {
            panic!("initial snapshot must be applied");
        };
        assert_eq!(first.revision, 1);

        let fresh = CredentialRuntimeStateSnapshot {
            state: CredentialRuntimeStateRow {
                failure_count: 2,
                refresh_failure_count: 1,
                disabled_reason: Some("TooManyFailures".to_string()),
                warmup_remaining: 3,
                revision: first.revision,
                ..Default::default()
            },
            expected_revision: first.revision,
        };
        let fresh = store
            .save_credential_runtime_state_snapshot(credential_id, &fresh)
            .await
            .unwrap();
        let CredentialRuntimeStateCasOutcome::Applied(fresh) = fresh else {
            panic!("fresh snapshot must be applied");
        };
        assert_eq!(fresh.revision, 2);

        let stale = CredentialRuntimeStateSnapshot {
            state: CredentialRuntimeStateRow {
                failure_count: 99,
                refresh_failure_count: 99,
                disabled_reason: None,
                warmup_remaining: 99,
                revision: 1,
                ..Default::default()
            },
            expected_revision: 1,
        };
        let conflict = store
            .save_credential_runtime_state_snapshot(credential_id, &stale)
            .await
            .unwrap();
        assert_eq!(
            conflict,
            CredentialRuntimeStateCasOutcome::Conflict {
                current: Some(fresh.clone())
            }
        );
        let stored = store
            .load_credential_runtime_state()
            .await
            .unwrap()
            .remove(&credential_id)
            .unwrap();
        assert_eq!(stored, fresh);

        let missing_state_id = store
            .insert_credential(&KiroCredentials {
                kiro_api_key: Some("ksk_runtime_snapshot_missing".to_string()),
                auth_method: Some("api_key".to_string()),
                ..Default::default()
            })
            .await
            .unwrap()
            .id
            .unwrap();
        let missing_state_snapshot = CredentialRuntimeStateSnapshot {
            state: CredentialRuntimeStateRow {
                revision: 7,
                ..CredentialRuntimeStateRow::default()
            },
            expected_revision: 7,
        };
        assert_eq!(
            store
                .save_credential_runtime_state_snapshot(missing_state_id, &missing_state_snapshot,)
                .await
                .unwrap(),
            CredentialRuntimeStateCasOutcome::Conflict { current: None }
        );
        assert!(
            !store
                .load_credential_runtime_state()
                .await
                .unwrap()
                .contains_key(&missing_state_id)
        );

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_runtime_snapshot_cas_allows_one_concurrent_writer() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;
        let credential_id = store
            .insert_credential(&KiroCredentials {
                kiro_api_key: Some("ksk_runtime_snapshot_concurrent".to_string()),
                auth_method: Some("api_key".to_string()),
                ..Default::default()
            })
            .await
            .unwrap()
            .id
            .unwrap();
        let initial = CredentialRuntimeStateRow {
            failure_count: 0,
            refresh_failure_count: 0,
            disabled_reason: None,
            warmup_remaining: 0,
            revision: 0,
            ..Default::default()
        };
        let initial = store
            .save_credential_runtime_state_for(credential_id, &initial)
            .await
            .unwrap();
        let CredentialRuntimeStateCasOutcome::Applied(initial) = initial else {
            panic!("initial state must be applied");
        };
        assert_eq!(initial.revision, 1);

        let first_snapshot = CredentialRuntimeStateSnapshot {
            state: CredentialRuntimeStateRow {
                failure_count: 10,
                revision: initial.revision,
                ..initial.clone()
            },
            expected_revision: initial.revision,
        };
        let second_snapshot = CredentialRuntimeStateSnapshot {
            state: CredentialRuntimeStateRow {
                failure_count: 20,
                revision: initial.revision,
                ..initial
            },
            expected_revision: 1,
        };
        let first_store = store.clone();
        let second_store = store.clone();
        let (first, second) = tokio::join!(
            first_store.save_credential_runtime_state_snapshot(credential_id, &first_snapshot,),
            second_store.save_credential_runtime_state_snapshot(credential_id, &second_snapshot,),
        );
        let outcomes = [first.unwrap(), second.unwrap()];
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, CredentialRuntimeStateCasOutcome::Applied(_)))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(
                    outcome,
                    CredentialRuntimeStateCasOutcome::Conflict { current: Some(_) }
                ))
                .count(),
            1
        );
        let stored = store
            .load_credential_runtime_state()
            .await
            .unwrap()
            .remove(&credential_id)
            .unwrap();
        assert_eq!(stored.revision, 2);
        assert!(matches!(stored.failure_count, 10 | 20));
        for outcome in outcomes {
            match outcome {
                CredentialRuntimeStateCasOutcome::Applied(state)
                | CredentialRuntimeStateCasOutcome::Conflict {
                    current: Some(state),
                } => assert_eq!(state, stored),
                CredentialRuntimeStateCasOutcome::Conflict { current: None } => {
                    panic!("existing state conflict must return the authoritative row")
                }
            }
        }

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_runtime_mutation_cleanup_is_expiry_aware_and_bounded() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;
        let credential_id = store
            .insert_credential(&KiroCredentials {
                kiro_api_key: Some("ksk_runtime_mutation_cleanup".to_string()),
                auth_method: Some("api_key".to_string()),
                ..Default::default()
            })
            .await
            .unwrap()
            .id
            .unwrap();
        let operation_ids = (0..4).map(|_| Uuid::new_v4()).collect::<Vec<_>>();
        for operation_id in &operation_ids {
            store
                .record_credential_success(credential_id, *operation_id)
                .await
                .unwrap();
        }
        sqlx::query(
            r#"
            UPDATE credential_runtime_mutations
            SET created_at = now() - interval '2 days'
            WHERE operation_id IN ($1, $2, $3)
            "#,
        )
        .bind(operation_ids[0].to_string())
        .bind(operation_ids[1].to_string())
        .bind(operation_ids[2].to_string())
        .execute(store.pool())
        .await
        .unwrap();

        let retention = std::time::Duration::from_secs(24 * 60 * 60);
        assert_eq!(
            store
                .cleanup_credential_runtime_mutations(retention, 2)
                .await
                .unwrap(),
            2
        );
        let remaining_expired: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*)::bigint
            FROM credential_runtime_mutations
            WHERE created_at < now() - interval '1 day'
            "#,
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(remaining_expired, 1);

        assert_eq!(
            store
                .cleanup_credential_runtime_mutations(retention, 1)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .cleanup_credential_runtime_mutations(retention, 1)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .cleanup_credential_runtime_mutations(retention, 0)
                .await
                .unwrap(),
            0
        );
        let remaining_ids: Vec<String> = sqlx::query_scalar(
            "SELECT operation_id FROM credential_runtime_mutations ORDER BY operation_id",
        )
        .fetch_all(store.pool())
        .await
        .unwrap();
        assert_eq!(remaining_ids, vec![operation_ids[3].to_string()]);

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_consistent_credential_runtime_load_matches_individual_loads() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;
        let first_id = store
            .insert_credential(&KiroCredentials {
                email: Some("consistent-first@example.com".to_string()),
                kiro_api_key: Some("ksk_consistent_first".to_string()),
                auth_method: Some("api_key".to_string()),
                priority: 2,
                disabled: true,
                ..Default::default()
            })
            .await
            .unwrap()
            .id
            .unwrap();
        store
            .insert_credential(&KiroCredentials {
                email: Some("consistent-second@example.com".to_string()),
                kiro_api_key: Some("ksk_consistent_second".to_string()),
                auth_method: Some("api_key".to_string()),
                priority: 1,
                ..Default::default()
            })
            .await
            .unwrap();
        let runtime_state_write = store
            .save_credential_runtime_state_for(
                first_id,
                &CredentialRuntimeStateRow {
                    failure_count: 2,
                    refresh_failure_count: 1,
                    disabled_reason: Some("TooManyFailures".to_string()),
                    warmup_remaining: 3,
                    revision: 0,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            runtime_state_write,
            CredentialRuntimeStateCasOutcome::Applied(_)
        ));

        let individual_credentials = store.load_credentials().await.unwrap();
        let individual_states = store.load_credential_runtime_state().await.unwrap();
        let (consistent_credentials, consistent_states) =
            store.load_credentials_with_runtime_state().await.unwrap();

        assert_eq!(consistent_credentials.len(), individual_credentials.len());
        for (consistent, individual) in consistent_credentials
            .iter()
            .zip(individual_credentials.iter())
        {
            assert_eq!(consistent.id, individual.id);
            assert_eq!(consistent.created_at, individual.created_at);
            assert_eq!(consistent.updated_at, individual.updated_at);
            assert_eq!(
                serde_json::to_value(consistent).unwrap(),
                serde_json::to_value(individual).unwrap()
            );
        }
        assert_eq!(consistent_states.len(), individual_states.len());
        for (credential_id, individual) in individual_states {
            let consistent = consistent_states.get(&credential_id).unwrap();
            assert_eq!(consistent.failure_count, individual.failure_count);
            assert_eq!(
                consistent.refresh_failure_count,
                individual.refresh_failure_count
            );
            assert_eq!(consistent.disabled_reason, individual.disabled_reason);
            assert_eq!(consistent.warmup_remaining, individual.warmup_remaining);
            assert_eq!(consistent.generation, individual.generation);
            assert_eq!(consistent.revision, individual.revision);
        }

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_runtime_revision_migration_upgrades_existing_rows() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;
        let credential_id = store
            .insert_credential(&KiroCredentials {
                kiro_api_key: Some("ksk_runtime_revision_migration".to_string()),
                auth_method: Some("api_key".to_string()),
                ..Default::default()
            })
            .await
            .unwrap()
            .id
            .unwrap();
        sqlx::query(
            "DELETE FROM schema_migrations WHERE version = 'credential-runtime-revision-v1'",
        )
        .execute(store.pool())
        .await
        .unwrap();
        sqlx::query("DROP TABLE credential_runtime_mutations")
            .execute(store.pool())
            .await
            .unwrap();
        sqlx::query("ALTER TABLE credential_runtime_state DROP COLUMN revision")
            .execute(store.pool())
            .await
            .unwrap();
        sqlx::query(
            r#"
            INSERT INTO credential_runtime_state (
                credential_id, failure_count, refresh_failure_count,
                disabled_reason, warmup_remaining, updated_at
            )
            VALUES ($1, 2, 1, NULL, 0, now())
            "#,
        )
        .bind(credential_id as i64)
        .execute(store.pool())
        .await
        .unwrap();

        store.migrate_with_options(false).await.unwrap();

        let state = store
            .load_credential_runtime_state()
            .await
            .unwrap()
            .remove(&credential_id)
            .unwrap();
        assert_eq!(state.failure_count, 2);
        assert_eq!(state.refresh_failure_count, 1);
        assert_eq!(state.revision, 1);
        let mutation_table_exists: Option<String> =
            sqlx::query_scalar("SELECT to_regclass('credential_runtime_mutations')::text")
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(
            mutation_table_exists.as_deref(),
            Some("credential_runtime_mutations")
        );

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_runtime_generation_migration_upgrades_existing_rows() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;
        let credential_id = store
            .insert_credential(&KiroCredentials {
                kiro_api_key: Some("ksk_runtime_generation_migration".to_string()),
                auth_method: Some("api_key".to_string()),
                ..Default::default()
            })
            .await
            .unwrap()
            .id
            .unwrap();
        sqlx::query(
            r#"
            INSERT INTO credential_runtime_state (
                credential_id, failure_count, refresh_failure_count,
                disabled_reason, warmup_remaining, generation, revision, updated_at
            )
            VALUES ($1, 2, 1, NULL, 0, 0, 1, now())
            "#,
        )
        .bind(credential_id as i64)
        .execute(store.pool())
        .await
        .unwrap();
        sqlx::query(
            "DELETE FROM schema_migrations WHERE version = 'credential-runtime-generation-v1'",
        )
        .execute(store.pool())
        .await
        .unwrap();
        sqlx::query("ALTER TABLE credential_runtime_state DROP COLUMN generation")
            .execute(store.pool())
            .await
            .unwrap();

        store.migrate_with_options(false).await.unwrap();

        let state = store
            .load_credential_runtime_state()
            .await
            .unwrap()
            .remove(&credential_id)
            .unwrap();
        assert_eq!(state.failure_count, 2);
        assert_eq!(state.refresh_failure_count, 1);
        assert_eq!(state.generation, 0);
        assert_eq!(state.revision, 1);
        let migration_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM schema_migrations WHERE version = 'credential-runtime-generation-v1'",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(migration_count, 1);

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_generates_unique_ids_for_concurrent_credential_inserts() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;

        let mut handles = Vec::new();
        for index in 0..20 {
            let store = store.clone();
            handles.push(tokio::spawn(async move {
                store
                    .insert_credential(&KiroCredentials {
                        email: Some(format!("concurrent-{}@example.com", index)),
                        kiro_api_key: Some(format!("ksk_concurrent_{}", index)),
                        auth_method: Some("api_key".to_string()),
                        priority: index,
                        ..Default::default()
                    })
                    .await
                    .unwrap()
                    .id
                    .unwrap()
            }));
        }

        let mut ids = Vec::new();
        for handle in handles {
            ids.push(handle.await.unwrap());
        }
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 20);

        let env_first = store
            .ensure_api_key_credential("ksk_env_import")
            .await
            .unwrap();
        let env_second = store
            .ensure_api_key_credential("ksk_env_import")
            .await
            .unwrap();
        assert_eq!(env_first.id, env_second.id);

        let duplicate = store
            .insert_credential(&KiroCredentials {
                email: Some("duplicate@example.com".to_string()),
                kiro_api_key: Some("ksk_concurrent_0".to_string()),
                auth_method: Some("api_key".to_string()),
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert!(duplicate.to_string().contains("kiroApiKey 重复"));

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_explicit_credential_ids_advance_sequence_without_regression() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;

        store
            .save_credentials(&[KiroCredentials {
                id: Some(7),
                email: Some("explicit-seven@example.com".to_string()),
                kiro_api_key: Some("ksk_explicit_seven".to_string()),
                auth_method: Some("api_key".to_string()),
                ..Default::default()
            }])
            .await
            .unwrap();
        let after_explicit = store
            .insert_credential(&KiroCredentials {
                email: Some("after-explicit@example.com".to_string()),
                kiro_api_key: Some("ksk_after_explicit".to_string()),
                auth_method: Some("api_key".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(after_explicit.id.unwrap() > 7);

        sqlx::query("SELECT setval('credentials_id_seq', $1, true)")
            .bind(100_i64)
            .execute(store.pool())
            .await
            .unwrap();
        store
            .save_credentials(&[KiroCredentials {
                id: Some(50),
                email: Some("explicit-fifty@example.com".to_string()),
                kiro_api_key: Some("ksk_explicit_fifty".to_string()),
                auth_method: Some("api_key".to_string()),
                ..Default::default()
            }])
            .await
            .unwrap();
        let after_higher_sequence = store
            .insert_credential(&KiroCredentials {
                email: Some("after-higher-sequence@example.com".to_string()),
                kiro_api_key: Some("ksk_after_higher_sequence".to_string()),
                auth_method: Some("api_key".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(after_higher_sequence.id, Some(101));

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn postgres_serializes_explicit_and_automatic_credential_id_allocation() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        const EXPLICIT_INSERTS: usize = 8;
        const AUTOMATIC_INSERTS: usize = 16;
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;
        let barrier = Arc::new(tokio::sync::Barrier::new(
            EXPLICIT_INSERTS + AUTOMATIC_INSERTS,
        ));
        let mut handles = Vec::new();

        for index in 0..EXPLICIT_INSERTS {
            let store = store.clone();
            let barrier = barrier.clone();
            handles.push(tokio::spawn(async move {
                let id = ((index + 1) * 10_000) as u64;
                barrier.wait().await;
                let saved = store
                    .save_credentials(&[KiroCredentials {
                        id: Some(id),
                        email: Some(format!("explicit-concurrent-{index}@example.com")),
                        kiro_api_key: Some(format!("ksk_explicit_concurrent_{index}")),
                        auth_method: Some("api_key".to_string()),
                        ..Default::default()
                    }])
                    .await
                    .unwrap();
                saved[0].id.unwrap()
            }));
        }
        for index in 0..AUTOMATIC_INSERTS {
            let store = store.clone();
            let barrier = barrier.clone();
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                store
                    .insert_credential(&KiroCredentials {
                        email: Some(format!("automatic-concurrent-{index}@example.com")),
                        kiro_api_key: Some(format!("ksk_automatic_concurrent_{index}")),
                        auth_method: Some("api_key".to_string()),
                        ..Default::default()
                    })
                    .await
                    .unwrap()
                    .id
                    .unwrap()
            }));
        }

        let mut ids = Vec::with_capacity(handles.len());
        for handle in handles {
            ids.push(handle.await.unwrap());
        }
        let inserted_count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), inserted_count);
        assert_eq!(
            store.load_credentials().await.unwrap().len(),
            inserted_count
        );
        let after_concurrent_inserts = store
            .insert_credential(&KiroCredentials {
                email: Some("after-concurrent-allocation@example.com".to_string()),
                kiro_api_key: Some("ksk_after_concurrent_allocation".to_string()),
                auth_method: Some("api_key".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(after_concurrent_inserts.id.unwrap() > (EXPLICIT_INSERTS * 10_000) as u64);

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_external_pool_list_and_get_preserve_body_modes() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;

        let created = store
            .create_external_pool(CreateExternalPoolRequest {
                name: "raw-pool".to_string(),
                base_url: "https://example.com".to_string(),
                api_key: "sk-test".to_string(),
                auth_type: ExternalPoolAuthType::Bearer,
                header_profile: ExternalPoolHeaderProfile::Generic,
                append_beta_query: false,
                header_overrides: HashMap::new(),
                wire_profile: ExternalPoolWireProfile::Default,
                tls_profile: ExternalPoolTlsProfile::Default,
                enabled: true,
                priority: 1,
                max_concurrent_requests: 2,
                usage_projection_mode: ExternalPoolUsageProjectionMode::PassThrough,
                stream_response_mode: Some(ExternalPoolStreamResponseMode::EventPassthrough),
                request_body_mode: ExternalPoolRequestBodyMode::RawPassthrough,
                raw_model_mode: ExternalPoolRawModelMode::RewriteTopLevel,
                auto_disable_policy: ExternalPoolAutoDisablePolicy::Inherit,
                pre_output_stream_retry_mode: ExternalPoolStreamRetryMode::Enabled,
                preserve_path: true,
                normalize_model_version_dots: false,
                model_mapping_mode: ExternalPoolModelMappingMode::PassthroughMapping,
                model_mapping_require_match: false,
                model_mapping_rules: vec![ModelMappingRule {
                    enabled: true,
                    source: "client-model".to_string(),
                    target: "mapped-model".to_string(),
                    kind: Default::default(),
                    note: None,
                }],
                supported_models: Vec::new(),
                route_mode: ExternalPoolRouteMode::AllowList,
                route_rules: vec!["/CC".to_string(), "/ha".to_string()],
                notes: None,
            })
            .await
            .unwrap();

        assert_eq!(
            created.request_body_mode,
            ExternalPoolRequestBodyMode::RawPassthrough
        );
        assert_eq!(
            created.raw_model_mode,
            ExternalPoolRawModelMode::RewriteTopLevel
        );
        assert_eq!(
            created.stream_response_mode,
            Some(ExternalPoolStreamResponseMode::EventPassthrough)
        );
        assert_eq!(
            created.pre_output_stream_retry_mode,
            ExternalPoolStreamRetryMode::Enabled
        );
        assert_eq!(created.route_mode, ExternalPoolRouteMode::AllowList);
        assert_eq!(
            created.route_rules,
            vec!["/cc".to_string(), "/ha".to_string()]
        );

        let listed = store.list_external_pools(false).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].request_body_mode,
            ExternalPoolRequestBodyMode::RawPassthrough
        );
        assert_eq!(
            listed[0].raw_model_mode,
            ExternalPoolRawModelMode::RewriteTopLevel
        );
        assert_eq!(
            listed[0].stream_response_mode,
            Some(ExternalPoolStreamResponseMode::EventPassthrough)
        );
        assert_eq!(
            listed[0].pre_output_stream_retry_mode,
            ExternalPoolStreamRetryMode::Enabled
        );

        let loaded = store
            .get_external_pool(created.id, false)
            .await
            .unwrap()
            .expect("pool should exist");
        assert_eq!(
            loaded.request_body_mode,
            ExternalPoolRequestBodyMode::RawPassthrough
        );
        assert_eq!(
            loaded.raw_model_mode,
            ExternalPoolRawModelMode::RewriteTopLevel
        );
        assert_eq!(
            loaded.stream_response_mode,
            Some(ExternalPoolStreamResponseMode::EventPassthrough)
        );
        assert_eq!(
            loaded.pre_output_stream_retry_mode,
            ExternalPoolStreamRetryMode::Enabled
        );
        assert_eq!(loaded.route_mode, ExternalPoolRouteMode::AllowList);
        assert_eq!(
            loaded.route_rules,
            vec!["/cc".to_string(), "/ha".to_string()]
        );

        let updated = store
            .update_external_pool(
                created.id,
                UpdateExternalPoolRequest {
                    stream_response_mode: Some(Some(
                        ExternalPoolStreamResponseMode::EventPassthrough,
                    )),
                    request_body_mode: Some(ExternalPoolRequestBodyMode::RawPassthrough),
                    raw_model_mode: Some(ExternalPoolRawModelMode::None),
                    pre_output_stream_retry_mode: Some(ExternalPoolStreamRetryMode::Disabled),
                    route_mode: Some(ExternalPoolRouteMode::DenyList),
                    route_rules: Some(vec!["/V1".to_string()]),
                    ..UpdateExternalPoolRequest::default()
                },
            )
            .await
            .unwrap()
            .expect("pool should update");
        assert_eq!(
            updated.request_body_mode,
            ExternalPoolRequestBodyMode::RawPassthrough
        );
        assert_eq!(updated.raw_model_mode, ExternalPoolRawModelMode::None);
        assert_eq!(
            updated.stream_response_mode,
            Some(ExternalPoolStreamResponseMode::EventPassthrough)
        );
        assert_eq!(
            updated.pre_output_stream_retry_mode,
            ExternalPoolStreamRetryMode::Disabled
        );
        assert_eq!(updated.route_mode, ExternalPoolRouteMode::DenyList);
        assert_eq!(updated.route_rules, vec!["/v1".to_string()]);

        let inherited = store
            .update_external_pool(
                created.id,
                UpdateExternalPoolRequest {
                    stream_response_mode: Some(None),
                    ..UpdateExternalPoolRequest::default()
                },
            )
            .await
            .unwrap()
            .expect("pool should clear stream override");
        assert_eq!(inherited.stream_response_mode, None);
        assert_eq!(
            inherited.pre_output_stream_retry_mode,
            ExternalPoolStreamRetryMode::Disabled
        );
        assert_eq!(inherited.route_mode, ExternalPoolRouteMode::DenyList);
        assert_eq!(inherited.route_rules, vec!["/v1".to_string()]);

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_rolls_up_external_pool_billing_for_large_samples_and_removes_after_cleanup() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;
        let usage_store = PostgresUsageStore::new(Arc::new(store.clone()));

        let records = (0..1000)
            .map(|index| {
                let (raw_cost, shaped_cost, uplifted_cost) = if index % 2 == 0 {
                    (0.010, 0.006, 0.008)
                } else {
                    (0.005, 0.007, 0.009)
                };
                external_usage_record(
                    &format!("external-usage-{index:04}"),
                    raw_cost,
                    shaped_cost,
                    uplifted_cost,
                )
            })
            .collect::<Vec<_>>();
        usage_store.record_batch(records).await.unwrap();

        let summary = usage_store.summary(1_000).await.unwrap();
        assert_eq!(summary.external_pool_billing.requests, 1000);
        assert_eq!(summary.external_pool_billing.priced_requests, 1000);
        assert_eq!(summary.external_pool_billing.unpriced_requests, 0);
        assert_eq!(
            summary.external_pool_billing.cost_floor_applied_requests,
            500
        );
        assert!((summary.external_pool_billing.raw_cost_usd - 7.5).abs() < 0.000001);
        assert!((summary.external_pool_billing.shaped_cost_usd - 6.5).abs() < 0.000001);
        assert!((summary.external_pool_billing.uplifted_cost_usd - 8.5).abs() < 0.000001);
        assert!((summary.external_pool_billing.profit_usd - 1.0).abs() < 0.000001);
        assert!((summary.external_pool_billing.reported_cost_usd - 8.5).abs() < 0.000001);
        assert!((summary.external_pool_billing.billable_cost_usd - 8.5).abs() < 0.000001);
        assert!((summary.external_pool_billing.cost_floor_delta_usd - 1.0).abs() < 0.000001);
        assert!((summary.total_estimated_cost_usd - 8.5).abs() < 0.000001);

        let dashboard = usage_store
            .dashboard(Some("Asia/Shanghai"), 1_000)
            .await
            .unwrap();
        let today = dashboard
            .windows
            .iter()
            .find(|window| window.key == "today")
            .unwrap();
        assert_eq!(today.summary.external_pool_billing.requests, 1000);
        assert_eq!(
            today
                .summary
                .external_pool_billing
                .cost_floor_applied_requests,
            500
        );
        assert!((today.summary.external_pool_billing.billable_cost_usd - 8.5).abs() < 0.000001);
        assert!((today.summary.external_pool_billing.profit_usd - 1.0).abs() < 0.000001);

        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        let cleanup_cutoff = Utc::now();
        let mut cleaned = 0;
        loop {
            let result = usage_store
                .soft_delete_cleanup_batch(cleanup_cutoff, 250)
                .await
                .unwrap();
            cleaned += result.processed_rows;
            if result.has_remaining == Some(false) || result.processed_rows == 0 {
                break;
            }
        }
        assert_eq!(cleaned, 1000);
        let cleared_page = usage_store
            .query(UsageRecordQuery {
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(cleared_page.total, 0);
        let cleared_summary = usage_store.summary(1_000).await.unwrap();
        assert_eq!(cleared_summary.external_pool_billing.requests, 0);
        assert_eq!(cleared_summary.external_pool_billing.priced_requests, 0);
        assert!(
            cleared_summary
                .external_pool_billing
                .billable_cost_usd
                .abs()
                < 0.000001
        );
        assert!(cleared_summary.external_pool_billing.profit_usd.abs() < 0.000001);
        assert!(cleared_summary.total_estimated_cost_usd.abs() < 0.000001);

        store.drop_test_schema().await.unwrap();
    }
}
