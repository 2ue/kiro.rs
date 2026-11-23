use crate::common::upstream_usage::RawUsageSource;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Datelike, Duration as ChronoDuration, TimeZone, Timelike, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{
    Connection, PgPool, Postgres, QueryBuilder, Row, Transaction,
    postgres::{PgConnection, PgPoolOptions, PgRow},
};

use crate::account::{
    Account, AccountAuthType, AccountEligibility, AccountHeaderProfile, AccountTlsProfile,
    AccountWireProfile, AutoDisablePolicy, CreateAccountRequest, ModelMappingMode, StreamRetryMode,
    UpdateAccountRequest, UsageProjectionMode, mask_account_key,
    normalize_account_header_overrides, normalize_account_model_mapping_rules,
};
use crate::anthropic::model_capabilities::{
    MANUAL_SOURCE, ModelCapabilitiesStatus, ModelCapabilityItem,
};
use crate::anthropic::pricing::{
    MANUAL_PRICING_SOURCE, ModelPriceItem, ModelPricing, PricingStatus,
};
use crate::anthropic::usage::{
    REALTIME_USAGE_WINDOW_SECS, USAGE_DEFAULT_ROUTE_KEY, UsageAccountBillingByPool,
    UsageAccountBillingSummary, UsageAccountLatencyResponse, UsageAccountLatencyRow,
    UsageAccountRiskBucket, UsageAccountRiskCacheStats, UsageAccountRiskCostConfig,
    UsageAccountRiskCostStats, UsageAccountRiskFilters, UsageAccountRiskGroup,
    UsageAccountRiskQuery, UsageAccountRiskResponse, UsageAccountRiskSample,
    UsageAccountRiskThresholds, UsageAccountRiskTotals, UsageAccountRiskWindow, UsageAggregate,
    UsageBreakdownItem, UsageDashboardResponse, UsageDashboardSeries, UsageDashboardSummary,
    UsageDashboardTop, UsageDashboardWindow, UsageDashboardWindowSpec, UsageOverviewExternal,
    UsageOverviewMetrics, UsageOverviewPoolRow, UsageOverviewRankRow, UsageOverviewResponse,
    UsageOverviewRouteRow, UsageOverviewSeriesBreakdown, UsageOverviewSeriesGroup,
    UsageOverviewSeriesGroups, UsageOverviewSeriesMetrics, UsageOverviewSeriesPoint,
    UsageOverviewSpec, UsageOverviewTotals, UsageRealtimeStats, UsageRecord, UsageRecordQuery,
    UsageRecordStatus, UsageRecordsPageResult, UsageRecordsResult, UsageRouteKind,
    UsageRouteLatencyResponse, UsageRouteLatencyRow, UsageSeriesPoint, UsageSource, UsageSummary,
    UsageTopAggregate, normalize_usage_route_filter, usage_dashboard_daily_windows,
    usage_dashboard_hourly_windows, usage_dashboard_timezone, usage_dashboard_window_spec_for_key,
    usage_dashboard_windows, usage_route_prefix_for_endpoint,
};
use crate::model::config::{
    Config, ModelMappingRule, RouteRuleMode, StreamResponseMode, normalize_route_rules,
};
use crate::model::model_support::normalize_supported_models;

const ACCOUNT_SELECT_SQL: &str = r#"
SELECT id, name, base_url, api_key, auth_type, enabled, priority,
       max_concurrent_requests, usage_projection_mode, stream_response_mode, raw_usage_source, model_filter_mode,
       auto_disable_policy, pre_output_stream_retry_mode,
       header_profile, append_beta_query, header_overrides,
       wire_profile, tls_profile,
       auto_disabled, auto_disabled_reason, auto_disabled_at,
       auto_disabled_until, auto_disabled_last_error,
       normalize_model_version_dots, model_mapping_mode,
       model_mapping_require_match, model_mapping_rules, supported_models,
       route_mode, route_rules, notes, tags, proxy_url, cost_multiplier, overrides,
       revision, created_at, updated_at
FROM accounts
WHERE deleted_at IS NULL
ORDER BY priority ASC, id ASC
"#;

const POSTGRES_MIGRATION_LOCK_ID: i64 = 4_950_531_234_001;
const USAGE_CLEANUP_COMMIT_LOCK_ID: i64 = 4_950_531_234_003;
const USAGE_OFFLINE_MAINTENANCE_LOCK_ID: i64 = 4_950_531_234_004;
const USAGE_RECORD_COMMIT_LOCK_DOMAIN: i64 = 0x0055_5341_4745_4944;
const USAGE_INDEX_STARTUP_MAX_BYTES: i64 = 64 * 1024 * 1024;
const ADMIN_AUDIT_CLEANUP_MAX_BATCH: usize = 5_000;
const ADMIN_AUDIT_CLEANUP_LOCK_TIMEOUT_MS: u64 = 250;
const ADMIN_AUDIT_CLEANUP_STATEMENT_TIMEOUT_MS: u64 = 5_000;
const USAGE_SOFT_DELETE_WATERMARK_SCOPE: &str = "soft_delete_created_at";
const USAGE_CLEANUP_LOCK_TIMEOUT_MS: u64 = 250;
/// Rows per batch for the automatic usage detail retention job.
const USAGE_RETENTION_MAX_BATCH: usize = 2_000;
/// Startup backfill of the overview total columns only runs when the detail table is small;
/// bigger deployments run `maintenance usage-overview-totals-backfill` off-peak.
const USAGE_OVERVIEW_TOTALS_STARTUP_MAX_BYTES: i64 = 256 * 1024 * 1024;
const USAGE_OVERVIEW_TOTALS_BACKFILL_VERSION: &str = "usage-overview-totals-backfill-v1";
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
        table_name: "accounts",
        column_name: "model_filter_mode",
    },
    RequiredPostgresColumn {
        table_name: "accounts",
        column_name: "raw_usage_source",
    },
    RequiredPostgresColumn {
        table_name: "accounts",
        column_name: "tags",
    },
    // 1.0.8 新表：migrate_on_start=false 时缺表要在启动时报出来，而不是采样写入静默失败。
    RequiredPostgresColumn {
        table_name: "account_quality_samples",
        column_name: "sample_count",
    },
    RequiredPostgresColumn {
        table_name: "accounts",
        column_name: "proxy_url",
    },
    RequiredPostgresColumn {
        table_name: "accounts",
        column_name: "cost_multiplier",
    },
    RequiredPostgresColumn {
        table_name: "accounts",
        column_name: "overrides",
    },
    RequiredPostgresColumn {
        table_name: "model_capabilities",
        column_name: "listed",
    },
    RequiredPostgresColumn {
        table_name: "usage_records",
        column_name: "account_cost_usd",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_totals",
        column_name: "total_compat_input_tokens",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_time_buckets",
        column_name: "total_compat_input_tokens",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_totals",
        column_name: "account_cost_usd",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_time_buckets",
        column_name: "account_raw_tokens",
    },
    RequiredPostgresColumn {
        table_name: "accounts",
        column_name: "revision",
    },
    RequiredPostgresColumn {
        table_name: "accounts",
        column_name: "pre_output_stream_retry_mode",
    },
    RequiredPostgresColumn {
        table_name: "accounts",
        column_name: "route_mode",
    },
    RequiredPostgresColumn {
        table_name: "accounts",
        column_name: "route_rules",
    },
    RequiredPostgresColumn {
        table_name: "accounts",
        column_name: "header_profile",
    },
    RequiredPostgresColumn {
        table_name: "accounts",
        column_name: "append_beta_query",
    },
    RequiredPostgresColumn {
        table_name: "accounts",
        column_name: "header_overrides",
    },
    RequiredPostgresColumn {
        table_name: "accounts",
        column_name: "wire_profile",
    },
    RequiredPostgresColumn {
        table_name: "accounts",
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
        table_name: "usage_cleanup_jobs",
        column_name: "include_summary",
    },
    RequiredPostgresColumn {
        table_name: "usage_cleanup_jobs",
        column_name: "max_rows",
    },
    RequiredPostgresColumn {
        table_name: "usage_records",
        column_name: "original_cost_usd",
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
        column_name: "upstream_metadata_requests",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_totals",
        column_name: "account_raw_cost_usd",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_totals",
        column_name: "account_shaped_cost_usd",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_totals",
        column_name: "account_uplifted_cost_usd",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_totals",
        column_name: "account_reported_cost_usd",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_totals",
        column_name: "account_billable_cost_usd",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_totals",
        column_name: "account_cost_floor_delta_usd",
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
        column_name: "upstream_metadata_requests",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_time_buckets",
        column_name: "account_raw_cost_usd",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_time_buckets",
        column_name: "account_shaped_cost_usd",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_time_buckets",
        column_name: "account_uplifted_cost_usd",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_time_buckets",
        column_name: "account_reported_cost_usd",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_time_buckets",
        column_name: "account_billable_cost_usd",
    },
    RequiredPostgresColumn {
        table_name: "usage_rollup_time_buckets",
        column_name: "account_cost_floor_delta_usd",
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
        name: "idx_usage_records_account_created",
        sql: "CREATE INDEX IF NOT EXISTS idx_usage_records_account_created ON usage_records (account_id, created_at DESC) WHERE deleted_at IS NULL",
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
        name: "idx_usage_records_account_json_created",
        sql: "CREATE INDEX IF NOT EXISTS idx_usage_records_account_json_created ON usage_records ((data->>'accountId'), created_at DESC) WHERE deleted_at IS NULL",
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
    // Account change view on the audit page: filter by account and by event kind. Built like the
    // usage indexes so a large audit table never stalls startup.
    UsageIndexDefinition {
        table: "admin_audit_logs",
        name: "idx_admin_audit_logs_object_created",
        sql: "CREATE INDEX IF NOT EXISTS idx_admin_audit_logs_object_created ON admin_audit_logs (object_type, object_id, created_at DESC)",
    },
    UsageIndexDefinition {
        table: "admin_audit_logs",
        name: "idx_admin_audit_logs_account_event_created",
        sql: "CREATE INDEX IF NOT EXISTS idx_admin_audit_logs_account_event_created ON admin_audit_logs ((detail->>'accountEvent'), created_at DESC) WHERE detail ? 'accountEvent'",
    },
];

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
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
        let pool = production_pool_options(config.postgres.max_connections)
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
        let pool = production_pool_options(config.postgres.usage_max_connections)
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
        let schema = format!("claude_pool_test_{}", uuid::Uuid::new_v4().simple());
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
                        .execute(&mut *conn)
                        .await?;
                    sqlx::query(&format!(
                        "SET idle_in_transaction_session_timeout = {IDLE_IN_TRANSACTION_SESSION_TIMEOUT_MS}"
                    ))
                    .execute(&mut *conn)
                    .await?;
                    Ok(())
                })
            })
            .after_release(|conn, _meta| Box::pin(async move { outside_transaction(conn).await }))
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
                        .execute(&mut *conn)
                        .await?;
                    sqlx::query(&format!(
                        "SET idle_in_transaction_session_timeout = {IDLE_IN_TRANSACTION_SESSION_TIMEOUT_MS}"
                    ))
                    .execute(&mut *conn)
                    .await?;
                    Ok(())
                })
            })
            .after_release(|conn, _meta| Box::pin(async move { outside_transaction(conn).await }))
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
                        .execute(&mut *conn)
                        .await?;
                    sqlx::query(&format!(
                        "SET idle_in_transaction_session_timeout = {IDLE_IN_TRANSACTION_SESSION_TIMEOUT_MS}"
                    ))
                    .execute(&mut *conn)
                    .await?;
                    Ok(())
                })
            })
            .after_release(|conn, _meta| Box::pin(async move { outside_transaction(conn).await }))
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
        let raw_source_constraint_current: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_constraint WHERE conrelid = 'accounts'::regclass AND conname = 'accounts_raw_usage_source_check' AND pg_get_constraintdef(oid) LIKE '%upstream_last_max_cost%')"
        ).fetch_one(&self.pool).await?;
        if !raw_source_constraint_current {
            missing.push("accounts.raw_usage_source_check".to_string());
        }

        if !self
            .usage_cleanup_batch_size_constraint_is_current()
            .await?
        {
            missing.push("usage_cleanup_jobs.batch_size_check<=10000".to_string());
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
            "PostgreSQL schema is not compatible with this claude-pool binary; missing required tables/columns: {}. {}. Set CLAUDE_POOL_POSTGRES_MIGRATE_ON_START=true or postgres.migrateOnStart=true, restart once to migrate the database, and do not run the service with an old or partial schema.",
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
            SELECT COALESCE(bool_or(pg_get_constraintdef(c.oid) LIKE '%<= 10000%'), false)
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
        let migration_lock_id = postgres_migration_lock_id(&mut conn).await?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(migration_lock_id)
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
                backfill_usage_overview_totals_if_startup_safe(&mut tx).await?;
                run_versioned_migration_in_tx(
                    &mut tx,
                    "usage-cleanup-batch-size-limit-v1",
                    USAGE_CLEANUP_BATCH_SIZE_LIMIT_SQL,
                )
                .await?;
                run_versioned_migration_in_tx(
                    &mut tx,
                    "usage-cleanup-batch-size-limit-v2",
                    USAGE_CLEANUP_BATCH_SIZE_LIMIT_V2_SQL,
                )
                .await?;
                run_versioned_migration_in_tx(
                    &mut tx,
                    "usage-cleanup-semantics-v2",
                    USAGE_CLEANUP_SEMANTICS_SQL,
                )
                .await?;
                run_versioned_migration_in_tx(
                    &mut tx,
                    "account-default-model-mapping-passthrough-v1",
                    ACCOUNT_DEFAULT_MODEL_MAPPING_PASSTHROUGH_SQL,
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
            if compress_usage_rollups && !usage_rollup_hour_compression_applied(&mut conn).await? {
                run_usage_exclusive_versioned_migration(
                    &mut conn,
                    "usage-rollup-hour-bucket-compression-v3",
                    USAGE_ROLLUP_HOUR_BUCKET_COMPRESSION_V3_SQL,
                )
                .await?;
            }
            Ok::<(), anyhow::Error>(())
        }
        .await;

        let unlock_result = sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(migration_lock_id)
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
        let migration_lock_id = postgres_migration_lock_id(&mut conn).await?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(migration_lock_id)
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
            .bind(migration_lock_id)
            .execute(&mut *conn)
            .await;
        result?;
        unlock_result?;
        Ok(())
    }

    pub async fn backfill_usage_legacy_cost_fields(&self) -> anyhow::Result<()> {
        let mut conn = self.pool.acquire().await?;
        let migration_lock_id = postgres_migration_lock_id(&mut conn).await?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(migration_lock_id)
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
            .bind(migration_lock_id)
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
        let mut value: serde_json::Value = row.try_get("config")?;
        let previous_migration = value
            .get("runtimeConfigMigrationVersion")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as u32;
        let migrated = crate::model::routes::migrate_path_overrides_to_routes(&mut value);
        let mut config: Config = serde_json::from_value(value)?;
        if migrated {
            config.runtime_config_migration_version = previous_migration.min(1);
        }
        config.set_config_path_for_runtime(None);
        // 升级过程中旧版本实例可能写回旧的提示词字段；每次加载都转换，热加载也能拿到新结构。
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
        config
            .routes
            .validate_with_config(config)
            .map_err(anyhow::Error::msg)?;
        let mut normalized = config.clone();
        for route in &mut normalized.routes.routes {
            route.prefix = crate::model::routes::normalize_route_prefix(&route.prefix);
        }
        let value = serde_json::to_value(normalized)?;
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

    pub async fn replace_configuration_transaction(
        &self,
        config: &Config,
        accounts: &[serde_json::Value],
        scenarios: &[serde_json::Value],
        pricing: &[serde_json::Value],
        capabilities: &[serde_json::Value],
    ) -> anyhow::Result<i64> {
        config
            .routes
            .validate_with_config(config)
            .map_err(anyhow::Error::msg)?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("LOCK TABLE accounts, runtime_config, route_scenarios, model_pricing, model_capabilities IN EXCLUSIVE MODE")
            .execute(&mut *tx).await?;
        let mut account_ids = Vec::new();
        for value in accounts {
            let request: CreateAccountRequest = serde_json::from_value(serde_json::Value::Object(
                value
                    .as_object()
                    .ok_or_else(|| anyhow::anyhow!("账号必须为对象"))?
                    .iter()
                    .map(|(key, value)| {
                        let mut parts = key.split('_');
                        let mut name = parts.next().unwrap_or_default().to_string();
                        for part in parts {
                            let mut chars = part.chars();
                            if let Some(first) = chars.next() {
                                name.extend(first.to_uppercase());
                                name.extend(chars);
                            }
                        }
                        (name, value.clone())
                    })
                    .collect(),
            ))?;
            validate_account_input(
                &request.name,
                &request.base_url,
                request.max_concurrent_requests,
            )?;
            validate_account_route_rules(&request.route_rules)?;
            normalize_account_header_overrides(request.header_overrides)?;
            let existing: Option<i64> = sqlx::query_scalar("SELECT id FROM accounts WHERE name = $1 ORDER BY (deleted_at IS NULL) DESC, id LIMIT 1")
                .bind(&request.name).fetch_optional(&mut *tx).await?;
            let id = match existing {
                Some(id) => id,
                None => {
                    sqlx::query_scalar("SELECT nextval(pg_get_serial_sequence('accounts', 'id'))")
                        .fetch_one(&mut *tx)
                        .await?
                }
            };
            let mut record = value.clone();
            let fields = record.as_object_mut().unwrap();
            fields.insert("id".into(), serde_json::json!(id));
            fields.insert(
                "model_filter_mode".into(),
                serde_json::to_value(request.model_filter_mode)?,
            );
            fields.insert(
                "raw_usage_source".into(),
                serde_json::to_value(request.raw_usage_source)?,
            );
            // 只写明确允许的列；不导入旧表的状态、主键和机器配置。
            sqlx::query(r#"
                INSERT INTO accounts (
                    id, name, base_url, api_key, auth_type, header_profile, append_beta_query,
                    header_overrides, wire_profile, tls_profile, enabled, priority,
                    max_concurrent_requests, usage_projection_mode, stream_response_mode, raw_usage_source, model_filter_mode,
                    auto_disable_policy, pre_output_stream_retry_mode, normalize_model_version_dots,
                    model_mapping_mode, model_mapping_require_match, model_mapping_rules,
                    supported_models, route_mode, route_rules, notes
                ) SELECT id, name, base_url, api_key, auth_type, header_profile, append_beta_query,
                    header_overrides, wire_profile, tls_profile, enabled, priority,
                    max_concurrent_requests, usage_projection_mode, stream_response_mode, raw_usage_source, model_filter_mode,
                    auto_disable_policy, pre_output_stream_retry_mode, normalize_model_version_dots,
                    model_mapping_mode, model_mapping_require_match, model_mapping_rules,
                    supported_models, route_mode, route_rules, notes
                  FROM jsonb_populate_record(NULL::accounts, $1)
                ON CONFLICT (id) DO UPDATE SET
                    name=EXCLUDED.name, base_url=EXCLUDED.base_url, api_key=EXCLUDED.api_key,
                    auth_type=EXCLUDED.auth_type, header_profile=EXCLUDED.header_profile,
                    append_beta_query=EXCLUDED.append_beta_query, header_overrides=EXCLUDED.header_overrides,
                    wire_profile=EXCLUDED.wire_profile, tls_profile=EXCLUDED.tls_profile,
                    enabled=EXCLUDED.enabled, priority=EXCLUDED.priority,
                    max_concurrent_requests=EXCLUDED.max_concurrent_requests,
                    usage_projection_mode=EXCLUDED.usage_projection_mode,
                    raw_usage_source=EXCLUDED.raw_usage_source,
                    stream_response_mode=EXCLUDED.stream_response_mode,
                    auto_disable_policy=EXCLUDED.auto_disable_policy,
                    pre_output_stream_retry_mode=EXCLUDED.pre_output_stream_retry_mode,
                    normalize_model_version_dots=EXCLUDED.normalize_model_version_dots,
                    model_mapping_mode=EXCLUDED.model_mapping_mode,
                    model_mapping_require_match=EXCLUDED.model_mapping_require_match,
                    model_mapping_rules=EXCLUDED.model_mapping_rules, supported_models=EXCLUDED.supported_models,
                    model_filter_mode=EXCLUDED.model_filter_mode,
                    route_mode=EXCLUDED.route_mode, route_rules=EXCLUDED.route_rules, notes=EXCLUDED.notes,
                    tags='{}', proxy_url=NULL, cost_multiplier=1, overrides='{}',
                    auto_disabled=false, auto_disabled_reason=NULL, auto_disabled_at=NULL,
                    auto_disabled_until=NULL, auto_disabled_last_error=NULL, deleted_at=NULL,
                    revision=accounts.revision+1, updated_at=now()
            "#).bind(record).execute(&mut *tx).await?;
            account_ids.push(id);
        }
        sqlx::query("DELETE FROM accounts WHERE id <> ALL($1::bigint[])")
            .bind(&account_ids)
            .execute(&mut *tx)
            .await?;
        let mut scenario_ids = Vec::new();
        for value in scenarios {
            let name = value["name"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("场景缺少 name"))?;
            let existing: Option<i64> = sqlx::query_scalar(
                "SELECT id FROM route_scenarios WHERE name=$1 ORDER BY id LIMIT 1",
            )
            .bind(name)
            .fetch_optional(&mut *tx)
            .await?;
            let id = if let Some(id) = existing {
                sqlx::query("UPDATE route_scenarios SET description=$2, content=$3, revision=revision+1, updated_at=now() WHERE id=$1")
                    .bind(id).bind(value["description"].as_str()).bind(&value["content"]).execute(&mut *tx).await?;
                id
            } else {
                sqlx::query_scalar("INSERT INTO route_scenarios (name,description,content) VALUES ($1,$2,$3) RETURNING id")
                    .bind(name).bind(value["description"].as_str()).bind(&value["content"]).fetch_one(&mut *tx).await?
            };
            scenario_ids.push(id);
        }
        sqlx::query("DELETE FROM route_scenarios WHERE id <> ALL($1::bigint[])")
            .bind(&scenario_ids)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM model_pricing")
            .execute(&mut *tx)
            .await?;
        for value in pricing {
            sqlx::query(r#"
                INSERT INTO model_pricing (model,input_cost_per_token,output_cost_per_token,
                    cache_creation_input_token_cost,cache_creation_1h_input_token_cost,
                    cache_read_input_token_cost,source,source_url,last_synced_at,last_error,updated_at)
                SELECT model,input_cost_per_token,output_cost_per_token,cache_creation_input_token_cost,
                    COALESCE(cache_creation_1h_input_token_cost,0),cache_read_input_token_cost,
                    source,source_url,last_synced_at,last_error,COALESCE(updated_at,now())
                FROM jsonb_populate_record(NULL::model_pricing,$1)
            "#).bind(value).execute(&mut *tx).await?;
        }
        sqlx::query("DELETE FROM model_capabilities WHERE source='manual'")
            .execute(&mut *tx)
            .await?;
        for value in capabilities {
            sqlx::query(r#"
                INSERT INTO model_capabilities (model,display_name,description,max_input_tokens,
                    max_output_tokens,supports_prompt_caching,supported_input_types,source,
                    last_synced_at,last_error,listed,updated_at)
                SELECT model,display_name,description,max_input_tokens,max_output_tokens,
                    supports_prompt_caching,supported_input_types,source,last_synced_at,last_error,true,COALESCE(updated_at,now())
                FROM jsonb_populate_record(NULL::model_capabilities,$1)
                ON CONFLICT (model) DO UPDATE SET display_name=EXCLUDED.display_name,
                    description=EXCLUDED.description,max_input_tokens=EXCLUDED.max_input_tokens,
                    max_output_tokens=EXCLUDED.max_output_tokens,
                    supports_prompt_caching=EXCLUDED.supports_prompt_caching,
                    supported_input_types=EXCLUDED.supported_input_types,source=EXCLUDED.source,
                    last_synced_at=EXCLUDED.last_synced_at,last_error=EXCLUDED.last_error,
                    listed=true,updated_at=EXCLUDED.updated_at
            "#).bind(value).execute(&mut *tx).await?;
        }
        let version = sqlx::query_scalar(
            r#"
            INSERT INTO runtime_config (id,config,updated_at) VALUES ('default',$1,now())
            ON CONFLICT (id) DO UPDATE SET config=EXCLUDED.config,
                version=runtime_config.version+1,updated_at=now() RETURNING version
        "#,
        )
        .bind(serde_json::to_value(config)?)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(version)
    }

    pub async fn replace_current_configuration_transaction(
        &self,
        config: &Config,
        accounts: &[serde_json::Value],
        scenarios: &[serde_json::Value],
        pricing: &[serde_json::Value],
        capabilities: &[serde_json::Value],
    ) -> anyhow::Result<i64> {
        config
            .routes
            .validate_with_config(config)
            .map_err(anyhow::Error::msg)?;
        let mut config = config.clone();
        let mut scenarios = scenarios.to_vec();
        let mut id_map = std::collections::BTreeMap::new();
        let mut tx = self.pool.begin().await?;
        sqlx::query("LOCK TABLE accounts, runtime_config, route_scenarios, model_pricing, model_capabilities IN EXCLUSIVE MODE")
            .execute(&mut *tx).await?;
        let mut account_ids = Vec::new();
        for value in accounts {
            let request: CreateAccountRequest = serde_json::from_value(serde_json::Value::Object(
                value
                    .as_object()
                    .ok_or_else(|| anyhow::anyhow!("账号必须为对象"))?
                    .iter()
                    .map(|(key, value)| {
                        let mut parts = key.split('_');
                        let mut name = parts.next().unwrap_or_default().to_string();
                        for part in parts {
                            let mut chars = part.chars();
                            if let Some(first) = chars.next() {
                                name.extend(first.to_uppercase());
                                name.extend(chars);
                            }
                        }
                        (name, value.clone())
                    })
                    .collect(),
            ))?;
            validate_account_input(
                &request.name,
                &request.base_url,
                request.max_concurrent_requests,
            )?;
            validate_account_route_rules(&request.route_rules)?;
            normalize_account_header_overrides(request.header_overrides.clone())?;
            crate::account::validate_account_fields(
                request.proxy_url.as_deref(),
                request.cost_multiplier,
                &request.overrides,
            )
            .map_err(anyhow::Error::msg)?;
            let existing: Option<i64> = sqlx::query_scalar("SELECT id FROM accounts WHERE name = $1 ORDER BY (deleted_at IS NULL) DESC, id LIMIT 1")
                .bind(&request.name).fetch_optional(&mut *tx).await?;
            let id = match existing {
                Some(id) => id,
                None => {
                    sqlx::query_scalar("SELECT nextval(pg_get_serial_sequence('accounts', 'id'))")
                        .fetch_one(&mut *tx)
                        .await?
                }
            };
            if let Some(old) = value["id"].as_u64() {
                id_map.insert(old, id as u64);
            }
            let mut record = serde_json::json!({
                "tags": request.tags,
                "proxy_url": request.proxy_url,
                "cost_multiplier": request.cost_multiplier,
                "overrides": request.overrides,
                "name": request.name,
                "base_url": request.base_url,
                "api_key": request.api_key,
                "auth_type": request.auth_type,
                "header_profile": request.header_profile,
                "append_beta_query": request.append_beta_query,
                "header_overrides": request.header_overrides,
                "wire_profile": request.wire_profile,
                "tls_profile": request.tls_profile,
                "enabled": request.enabled,
                "priority": request.priority,
                "max_concurrent_requests": request.max_concurrent_requests,
                "usage_projection_mode": request.usage_projection_mode,
                "stream_response_mode": request.stream_response_mode,
                "auto_disable_policy": request.auto_disable_policy,
                "pre_output_stream_retry_mode": request.pre_output_stream_retry_mode,
                "normalize_model_version_dots": false,
                "model_mapping_mode": request.model_mapping_mode,
                "model_mapping_require_match": request.model_mapping_require_match,
                "model_mapping_rules": request.model_mapping_rules,
                "supported_models": request.supported_models,
                "model_filter_mode": request.model_filter_mode,
                "route_mode": request.route_mode,
                "route_rules": request.route_rules,
                "notes": request.notes
            });
            let fields = record.as_object_mut().unwrap();
            fields.insert("id".into(), serde_json::json!(id));
            fields.insert(
                "model_filter_mode".into(),
                serde_json::to_value(request.model_filter_mode)?,
            );
            fields.insert(
                "raw_usage_source".into(),
                serde_json::to_value(request.raw_usage_source)?,
            );
            // 只写明确允许的列；不导入旧表的状态、主键和机器配置。
            sqlx::query(r#"
                INSERT INTO accounts (
                    id, name, base_url, api_key, auth_type, header_profile, append_beta_query,
                    header_overrides, wire_profile, tls_profile, enabled, priority,
                    max_concurrent_requests, usage_projection_mode, stream_response_mode, raw_usage_source, model_filter_mode,
                    auto_disable_policy, pre_output_stream_retry_mode, normalize_model_version_dots,
                    model_mapping_mode, model_mapping_require_match, model_mapping_rules,
                    supported_models, route_mode, route_rules, notes, tags, proxy_url, cost_multiplier, overrides
                ) SELECT id, name, base_url, api_key, auth_type, header_profile, append_beta_query,
                    header_overrides, wire_profile, tls_profile, enabled, priority,
                    max_concurrent_requests, usage_projection_mode, stream_response_mode, raw_usage_source, model_filter_mode,
                    auto_disable_policy, pre_output_stream_retry_mode, normalize_model_version_dots,
                    model_mapping_mode, model_mapping_require_match, model_mapping_rules,
                    supported_models, route_mode, route_rules, notes, tags, proxy_url, cost_multiplier, overrides
                  FROM jsonb_populate_record(NULL::accounts, $1)
                ON CONFLICT (id) DO UPDATE SET
                    name=EXCLUDED.name, base_url=EXCLUDED.base_url, api_key=EXCLUDED.api_key,
                    auth_type=EXCLUDED.auth_type, header_profile=EXCLUDED.header_profile,
                    append_beta_query=EXCLUDED.append_beta_query, header_overrides=EXCLUDED.header_overrides,
                    wire_profile=EXCLUDED.wire_profile, tls_profile=EXCLUDED.tls_profile,
                    enabled=EXCLUDED.enabled, priority=EXCLUDED.priority,
                    max_concurrent_requests=EXCLUDED.max_concurrent_requests,
                    usage_projection_mode=EXCLUDED.usage_projection_mode,
                    raw_usage_source=EXCLUDED.raw_usage_source,
                    stream_response_mode=EXCLUDED.stream_response_mode,
                    auto_disable_policy=EXCLUDED.auto_disable_policy,
                    pre_output_stream_retry_mode=EXCLUDED.pre_output_stream_retry_mode,
                    normalize_model_version_dots=EXCLUDED.normalize_model_version_dots,
                    model_mapping_mode=EXCLUDED.model_mapping_mode,
                    model_mapping_require_match=EXCLUDED.model_mapping_require_match,
                    model_mapping_rules=EXCLUDED.model_mapping_rules, supported_models=EXCLUDED.supported_models,
                    model_filter_mode=EXCLUDED.model_filter_mode,
                    route_mode=EXCLUDED.route_mode, route_rules=EXCLUDED.route_rules, notes=EXCLUDED.notes,
                    tags=EXCLUDED.tags, proxy_url=EXCLUDED.proxy_url, cost_multiplier=EXCLUDED.cost_multiplier, overrides=EXCLUDED.overrides,
                    auto_disabled=false, auto_disabled_reason=NULL, auto_disabled_at=NULL,
                    auto_disabled_until=NULL, auto_disabled_last_error=NULL, deleted_at=NULL,
                    revision=accounts.revision+1, updated_at=now()
            "#).bind(record).execute(&mut *tx).await?;
            account_ids.push(id);
        }
        sqlx::query("DELETE FROM accounts WHERE id <> ALL($1::bigint[])")
            .bind(&account_ids)
            .execute(&mut *tx)
            .await?;
        remap_configuration_account_ids(&mut config, &mut scenarios, &id_map)?;
        let mut scenario_ids = Vec::new();
        for value in &scenarios {
            let name = value["name"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("场景缺少 name"))?;
            let existing: Option<i64> = sqlx::query_scalar(
                "SELECT id FROM route_scenarios WHERE name=$1 ORDER BY id LIMIT 1",
            )
            .bind(name)
            .fetch_optional(&mut *tx)
            .await?;
            let id = if let Some(id) = existing {
                sqlx::query("UPDATE route_scenarios SET description=$2, content=$3, revision=revision+1, updated_at=now() WHERE id=$1")
                    .bind(id).bind(value["description"].as_str()).bind(&value["content"]).execute(&mut *tx).await?;
                id
            } else {
                sqlx::query_scalar("INSERT INTO route_scenarios (name,description,content) VALUES ($1,$2,$3) RETURNING id")
                    .bind(name).bind(value["description"].as_str()).bind(&value["content"]).fetch_one(&mut *tx).await?
            };
            scenario_ids.push(id);
        }
        sqlx::query("DELETE FROM route_scenarios WHERE id <> ALL($1::bigint[])")
            .bind(&scenario_ids)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM model_pricing")
            .execute(&mut *tx)
            .await?;
        for value in pricing {
            sqlx::query(r#"
                INSERT INTO model_pricing (model,input_cost_per_token,output_cost_per_token,
                    cache_creation_input_token_cost,cache_creation_1h_input_token_cost,
                    cache_read_input_token_cost,source,source_url,last_synced_at,last_error,updated_at)
                SELECT model,input_cost_per_token,output_cost_per_token,cache_creation_input_token_cost,
                    COALESCE(cache_creation_1h_input_token_cost,0),cache_read_input_token_cost,
                    source,source_url,last_synced_at,last_error,COALESCE(updated_at,now())
                FROM jsonb_populate_record(NULL::model_pricing,$1)
            "#).bind(value).execute(&mut *tx).await?;
        }
        sqlx::query(
            "DELETE FROM model_capabilities WHERE source IN ('manual','account_discovery')",
        )
        .execute(&mut *tx)
        .await?;
        for value in capabilities {
            sqlx::query(r#"
                INSERT INTO model_capabilities (model,display_name,description,max_input_tokens,
                    max_output_tokens,supports_prompt_caching,supported_input_types,source,
                    last_synced_at,last_error,listed,updated_at)
                SELECT model,display_name,description,max_input_tokens,max_output_tokens,
                    supports_prompt_caching,supported_input_types,source,last_synced_at,last_error,COALESCE(listed,true),COALESCE(updated_at,now())
                FROM jsonb_populate_record(NULL::model_capabilities,$1)
                ON CONFLICT (model) DO UPDATE SET display_name=EXCLUDED.display_name,
                    description=EXCLUDED.description,max_input_tokens=EXCLUDED.max_input_tokens,
                    max_output_tokens=EXCLUDED.max_output_tokens,
                    supports_prompt_caching=EXCLUDED.supports_prompt_caching,
                    supported_input_types=EXCLUDED.supported_input_types,source=EXCLUDED.source,
                    last_synced_at=EXCLUDED.last_synced_at,last_error=EXCLUDED.last_error,
                    listed=EXCLUDED.listed,updated_at=EXCLUDED.updated_at
            "#).bind(value).execute(&mut *tx).await?;
        }
        let version = sqlx::query_scalar(
            r#"
            INSERT INTO runtime_config (id,config,updated_at) VALUES ('default',$1,now())
            ON CONFLICT (id) DO UPDATE SET config=EXCLUDED.config,
                version=runtime_config.version+1,updated_at=now() RETURNING version
        "#,
        )
        .bind(serde_json::to_value(&config)?)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(version)
    }

    pub async fn export_current_configuration(&self) -> anyhow::Result<serde_json::Value> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        let mut result = serde_json::Map::new();
        for (key, query) in [
            (
                "accounts",
                "SELECT COALESCE(jsonb_agg(to_jsonb(t) ORDER BY t.name), '[]'::jsonb) FROM accounts t WHERE deleted_at IS NULL",
            ),
            (
                "routeScenarios",
                "SELECT COALESCE(jsonb_agg(to_jsonb(t) ORDER BY t.name), '[]'::jsonb) FROM route_scenarios t",
            ),
            (
                "modelPricing",
                "SELECT COALESCE(jsonb_agg(to_jsonb(t) ORDER BY t.model), '[]'::jsonb) FROM model_pricing t",
            ),
            (
                "manualModels",
                "SELECT COALESCE(jsonb_agg(to_jsonb(t) ORDER BY t.model), '[]'::jsonb) FROM model_capabilities t WHERE source IN ('manual','account_discovery')",
            ),
        ] {
            let value: serde_json::Value = sqlx::query_scalar(query).fetch_one(&mut *tx).await?;
            result.insert(key.to_string(), value);
        }
        let config: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT config FROM runtime_config WHERE id='default'")
                .fetch_optional(&mut *tx)
                .await?;
        result.insert(
            "runtimeConfig".into(),
            config.unwrap_or(serde_json::to_value(Config::default())?),
        );
        tx.commit().await?;
        Ok(serde_json::Value::Object(result))
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

    pub async fn list_accounts(&self, mask_secrets: bool) -> anyhow::Result<Vec<Account>> {
        let rows = sqlx::query(ACCOUNT_SELECT_SQL)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| account_from_row(row, mask_secrets))
            .collect()
    }

    /// Loads only rows that can be decoded without compatibility defaults.
    /// A malformed pool is isolated from healthy pools and cannot authorize dispatch.
    pub async fn list_dispatchable_accounts(&self) -> anyhow::Result<Vec<Account>> {
        let rows = sqlx::query(ACCOUNT_SELECT_SQL)
            .fetch_all(&self.pool)
            .await?;
        let mut pools = Vec::with_capacity(rows.len());
        let mut invalid_pool_ids = Vec::new();
        let mut invalid_rows = 0usize;
        for row in rows {
            let pool_id = row.try_get::<i64, _>("id").ok();
            match account_from_row_with_policy(row, false, true) {
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
                "账号持久化配置无法安全解析，调度资格按池 fail closed"
            );
        }
        Ok(pools)
    }

    pub async fn list_account_eligibility(&self) -> anyhow::Result<Vec<AccountEligibility>> {
        let rows = sqlx::query(
            r#"
            SELECT id, revision, enabled, base_url,
                   (btrim(api_key) <> '') AS api_key_present,
                   auth_type, max_concurrent_requests, usage_projection_mode, raw_usage_source,
                   stream_response_mode,
                   auto_disable_policy, pre_output_stream_retry_mode,
                   header_profile, append_beta_query, header_overrides,
                   wire_profile, tls_profile, model_mapping_mode,
                   model_mapping_require_match, model_mapping_rules,
                   auto_disabled, auto_disabled_until, supported_models, model_filter_mode, normalize_model_version_dots,
                   route_mode, route_rules
            FROM accounts
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
            match account_eligibility_from_row(&row) {
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
                "账号静态资格配置无法安全解析，按池 fail closed"
            );
        }
        Ok(eligibility)
    }

    pub async fn get_account(
        &self,
        id: u64,
        mask_secrets: bool,
    ) -> anyhow::Result<Option<Account>> {
        let row = sqlx::query(
            r#"
            SELECT id, name, base_url, api_key, auth_type, enabled, priority,
                   max_concurrent_requests, usage_projection_mode, stream_response_mode, raw_usage_source, model_filter_mode,
                   auto_disable_policy, pre_output_stream_retry_mode,
                   header_profile, append_beta_query, header_overrides,
                   wire_profile, tls_profile,
                   auto_disabled, auto_disabled_reason, auto_disabled_at,
                   auto_disabled_until, auto_disabled_last_error,
                   normalize_model_version_dots, model_mapping_mode,
                   model_mapping_require_match, model_mapping_rules, supported_models,
                   route_mode, route_rules, notes, tags, proxy_url, cost_multiplier, overrides,
                   revision, created_at, updated_at
            FROM accounts
            WHERE id = $1 AND deleted_at IS NULL
            "#,
        )
        .bind(id as i64)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| account_from_row(row, mask_secrets))
            .transpose()
    }

    /// Defines the request's dispatch linearization point after request preparation and before
    /// attempt reservation/HTTP send. A mutation committed after this query does not revoke an
    /// already-started dispatch; callers must not hold a PostgreSQL lock across upstream I/O.
    pub async fn account_dispatch_revision_matches(
        &self,
        id: u64,
        expected_revision: u64,
    ) -> anyhow::Result<bool> {
        let matches: bool = sqlx::query_scalar(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM accounts
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
                .map_err(|_| anyhow::anyhow!("账号 revision 超出 PgSQL BIGINT 范围"))?,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(matches)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn create_account(&self, request: CreateAccountRequest) -> anyhow::Result<Account> {
        self.create_account_with_secret_policy(request, true).await
    }

    pub(crate) async fn create_account_unmasked(
        &self,
        request: CreateAccountRequest,
    ) -> anyhow::Result<Account> {
        self.create_account_with_secret_policy(request, false).await
    }

    async fn create_account_with_secret_policy(
        &self,
        request: CreateAccountRequest,
        mask_secrets: bool,
    ) -> anyhow::Result<Account> {
        validate_account_input(
            &request.name,
            &request.base_url,
            request.max_concurrent_requests,
        )?;
        crate::account::validate_account_fields(
            request.proxy_url.as_deref(),
            request.cost_multiplier,
            &request.overrides,
        )?;
        let model_mapping_rules =
            normalize_account_model_mapping_rules(request.model_mapping_rules);
        let model_mapping_rules_value = serde_json::to_value(&model_mapping_rules)?;
        let supported_models = normalize_supported_models(request.supported_models);
        let supported_models_value = serde_json::to_value(&supported_models)?;
        validate_account_route_rules(&request.route_rules)?;
        let route_rules = normalize_route_rules(&request.route_rules);
        let route_rules_value = serde_json::to_value(&route_rules)?;
        let header_overrides = normalize_account_header_overrides(request.header_overrides)?;
        let header_overrides_value = serde_json::to_value(&header_overrides)?;
        let row = sqlx::query(
            r#"
            INSERT INTO accounts (
                name, base_url, api_key, auth_type, enabled, priority,
                max_concurrent_requests, usage_projection_mode, stream_response_mode,
                auto_disable_policy, pre_output_stream_retry_mode,
                header_profile, append_beta_query, header_overrides,
                wire_profile, tls_profile,
                normalize_model_version_dots, model_mapping_mode,
                model_mapping_require_match, model_mapping_rules, supported_models,
                route_mode, route_rules, notes, tags, proxy_url, cost_multiplier, overrides, raw_usage_source, model_filter_mode, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24, $25, $26, $27, $28, $29, $30, now())
            RETURNING id, name, base_url, api_key, auth_type, enabled, priority,
                      max_concurrent_requests, usage_projection_mode, stream_response_mode, raw_usage_source, model_filter_mode,
                      auto_disable_policy, pre_output_stream_retry_mode,
                      header_profile, append_beta_query, header_overrides,
                      wire_profile, tls_profile,
                      auto_disabled, auto_disabled_reason, auto_disabled_at,
                      auto_disabled_until, auto_disabled_last_error,
                      normalize_model_version_dots, model_mapping_mode,
                      model_mapping_require_match, model_mapping_rules, supported_models,
                      route_mode, route_rules, notes, tags, proxy_url, cost_multiplier, overrides,
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
        .bind(request.auto_disable_policy.as_str())
        .bind(request.pre_output_stream_retry_mode.as_str())
        .bind(request.header_profile.as_str())
        .bind(request.append_beta_query)
        .bind(header_overrides_value)
        .bind(request.wire_profile.as_str())
        .bind(request.tls_profile.as_str())
        .bind(false)
        .bind(request.model_mapping_mode.as_str())
        .bind(request.model_mapping_require_match)
        .bind(model_mapping_rules_value)
        .bind(supported_models_value)
        .bind(request.route_mode.as_str())
        .bind(route_rules_value)
        .bind(request.notes.map(|notes| notes.trim().to_string()))
        .bind(&request.tags)
        .bind(request.proxy_url.as_deref().filter(|url| !url.trim().is_empty()))
        .bind(request.cost_multiplier)
        .bind(serde_json::to_value(&request.overrides)?)
        .bind(request.raw_usage_source.as_str())
        .bind(request.model_filter_mode.as_str())
        .fetch_one(&self.pool)
        .await?;
        account_from_row(row, mask_secrets)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn update_account(
        &self,
        id: u64,
        request: UpdateAccountRequest,
    ) -> anyhow::Result<Option<Account>> {
        self.update_account_with_secret_policy(id, request, true)
            .await
    }

    pub(crate) async fn update_account_unmasked(
        &self,
        id: u64,
        request: UpdateAccountRequest,
    ) -> anyhow::Result<Option<Account>> {
        self.update_account_with_secret_policy(id, request, false)
            .await
    }

    async fn update_account_with_secret_policy(
        &self,
        id: u64,
        request: UpdateAccountRequest,
        mask_secrets: bool,
    ) -> anyhow::Result<Option<Account>> {
        let Some(current) = self.get_account(id, false).await? else {
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
        let raw_usage_source = request.raw_usage_source.unwrap_or(current.raw_usage_source);
        let model_filter_mode = request
            .model_filter_mode
            .unwrap_or(current.model_filter_mode);
        let stream_response_mode = request
            .stream_response_mode
            .unwrap_or(current.stream_response_mode);
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
            .map(normalize_account_header_overrides)
            .transpose()?
            .unwrap_or(current.header_overrides);
        let header_overrides_value = serde_json::to_value(&header_overrides)?;
        let wire_profile = request.wire_profile.unwrap_or(current.wire_profile);
        let tls_profile = request.tls_profile.unwrap_or(current.tls_profile);
        let model_mapping_mode = request
            .model_mapping_mode
            .unwrap_or(current.model_mapping_mode);
        let model_mapping_require_match = request
            .model_mapping_require_match
            .unwrap_or(current.model_mapping_require_match);
        let model_mapping_rules = request
            .model_mapping_rules
            .map(normalize_account_model_mapping_rules)
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
                validate_account_route_rules(&rules)?;
                Ok::<_, anyhow::Error>(normalize_route_rules(&rules))
            })
            .transpose()?
            .unwrap_or(current.route_rules);
        let route_rules_value = serde_json::to_value(&route_rules)?;
        let notes = request.notes.or(current.notes);
        let tags = request.tags.unwrap_or(current.tags);
        let proxy_url = request
            .proxy_url
            .unwrap_or(current.proxy_url)
            .filter(|url| !url.trim().is_empty());
        let cost_multiplier = request.cost_multiplier.unwrap_or(current.cost_multiplier);
        let overrides = request.overrides.unwrap_or(current.overrides);
        crate::account::validate_account_fields(proxy_url.as_deref(), cost_multiplier, &overrides)?;
        validate_account_input(&name, &base_url, max_concurrent_requests)?;
        let row = sqlx::query(
            r#"
            UPDATE accounts
            SET name = $2,
                base_url = $3,
                api_key = $4,
                auth_type = $5,
                enabled = $6,
                priority = $7,
                max_concurrent_requests = $8,
                usage_projection_mode = $9,
                stream_response_mode = $10,
                auto_disable_policy = $11,
                pre_output_stream_retry_mode = $12,
                header_profile = $13,
                append_beta_query = $14,
                header_overrides = $15,
                wire_profile = $16,
                tls_profile = $17,
                normalize_model_version_dots = $18,
                model_mapping_mode = $19,
                model_mapping_require_match = $20,
                model_mapping_rules = $21,
                supported_models = $22,
                route_mode = $23,
                route_rules = $24,
                notes = $25,
                tags = $26, proxy_url = $27, cost_multiplier = $28, overrides = $29,
                raw_usage_source = $30,
                model_filter_mode = $31,
                revision = revision + 1,
                updated_at = now()
            WHERE id = $1 AND deleted_at IS NULL
            RETURNING id, name, base_url, api_key, auth_type, enabled, priority,
                      max_concurrent_requests, usage_projection_mode, stream_response_mode, raw_usage_source, model_filter_mode,
                      auto_disable_policy, pre_output_stream_retry_mode,
                      header_profile, append_beta_query, header_overrides,
                      wire_profile, tls_profile,
                      auto_disabled, auto_disabled_reason, auto_disabled_at,
                      auto_disabled_until, auto_disabled_last_error,
                      normalize_model_version_dots, model_mapping_mode,
                      model_mapping_require_match, model_mapping_rules, supported_models,
                      route_mode, route_rules, notes, tags, proxy_url, cost_multiplier, overrides,
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
        .bind(auto_disable_policy.as_str())
        .bind(pre_output_stream_retry_mode.as_str())
        .bind(header_profile.as_str())
        .bind(append_beta_query)
        .bind(header_overrides_value)
        .bind(wire_profile.as_str())
        .bind(tls_profile.as_str())
        .bind(false)
        .bind(model_mapping_mode.as_str())
        .bind(model_mapping_require_match)
        .bind(model_mapping_rules_value)
        .bind(supported_models_value)
        .bind(route_mode.as_str())
        .bind(route_rules_value)
        .bind(notes)
        .bind(tags)
        .bind(proxy_url)
        .bind(cost_multiplier)
        .bind(serde_json::to_value(&overrides)?)
        .bind(raw_usage_source.as_str())
        .bind(model_filter_mode.as_str())
        .fetch_one(&self.pool)
        .await?;
        Ok(Some(account_from_row(row, mask_secrets)?))
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn set_account_enabled(
        &self,
        id: u64,
        enabled: bool,
    ) -> anyhow::Result<Option<Account>> {
        self.set_account_enabled_with_secret_policy(id, enabled, true)
            .await
    }

    pub(crate) async fn set_account_enabled_unmasked(
        &self,
        id: u64,
        enabled: bool,
    ) -> anyhow::Result<Option<Account>> {
        self.set_account_enabled_with_secret_policy(id, enabled, false)
            .await
    }

    async fn set_account_enabled_with_secret_policy(
        &self,
        id: u64,
        enabled: bool,
        mask_secrets: bool,
    ) -> anyhow::Result<Option<Account>> {
        let row = sqlx::query(
            r#"
            UPDATE accounts
            SET enabled = $2,
                revision = revision + 1,
                updated_at = now()
            WHERE id = $1 AND deleted_at IS NULL
            RETURNING id, name, base_url, api_key, auth_type, enabled, priority,
                      max_concurrent_requests, usage_projection_mode, stream_response_mode, raw_usage_source, model_filter_mode,
                      auto_disable_policy, pre_output_stream_retry_mode,
                      header_profile, append_beta_query, header_overrides,
                      wire_profile, tls_profile,
                      auto_disabled, auto_disabled_reason, auto_disabled_at,
                      auto_disabled_until, auto_disabled_last_error,
                      normalize_model_version_dots, model_mapping_mode,
                      model_mapping_require_match, model_mapping_rules, supported_models,
                      route_mode, route_rules, notes, tags, proxy_url, cost_multiplier, overrides,
                      revision, created_at, updated_at
            "#,
        )
        .bind(id as i64)
        .bind(enabled)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| account_from_row(row, mask_secrets))
            .transpose()
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn set_account_supported_models(
        &self,
        id: u64,
        supported_models: Vec<String>,
    ) -> anyhow::Result<Option<Account>> {
        self.set_account_supported_models_with_secret_policy(id, supported_models, true)
            .await
    }

    async fn set_account_supported_models_with_secret_policy(
        &self,
        id: u64,
        supported_models: Vec<String>,
        mask_secrets: bool,
    ) -> anyhow::Result<Option<Account>> {
        let supported_models = normalize_supported_models(supported_models);
        let supported_models_value = serde_json::to_value(&supported_models)?;
        let row = sqlx::query(
            r#"
            UPDATE accounts
            SET supported_models = $2,
                revision = revision + 1,
                updated_at = now()
            WHERE id = $1 AND deleted_at IS NULL
            RETURNING id, name, base_url, api_key, auth_type, enabled, priority,
                      max_concurrent_requests, usage_projection_mode, stream_response_mode, raw_usage_source, model_filter_mode,
                      auto_disable_policy, pre_output_stream_retry_mode,
                      header_profile, append_beta_query, header_overrides,
                      wire_profile, tls_profile,
                      auto_disabled, auto_disabled_reason, auto_disabled_at,
                      auto_disabled_until, auto_disabled_last_error,
                      normalize_model_version_dots, model_mapping_mode,
                      model_mapping_require_match, model_mapping_rules, supported_models,
                      route_mode, route_rules, notes, tags, proxy_url, cost_multiplier, overrides,
                      revision, created_at, updated_at
            "#,
        )
        .bind(id as i64)
        .bind(supported_models_value)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| account_from_row(row, mask_secrets))
            .transpose()
    }

    pub async fn soft_delete_account(&self, id: u64) -> anyhow::Result<bool> {
        let result = sqlx::query(
            "UPDATE accounts SET deleted_at = now(), revision = revision + 1, updated_at = now() WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(id as i64)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn clear_account_auto_disabled(&self, id: u64) -> anyhow::Result<Option<Account>> {
        self.clear_account_auto_disabled_with_secret_policy(id, true)
            .await
    }

    pub(crate) async fn clear_account_auto_disabled_unmasked(
        &self,
        id: u64,
    ) -> anyhow::Result<Option<Account>> {
        self.clear_account_auto_disabled_with_secret_policy(id, false)
            .await
    }

    async fn clear_account_auto_disabled_with_secret_policy(
        &self,
        id: u64,
        mask_secrets: bool,
    ) -> anyhow::Result<Option<Account>> {
        let row = sqlx::query(
            r#"
            UPDATE accounts
            SET auto_disabled = false,
                auto_disabled_reason = NULL,
                auto_disabled_at = NULL,
                auto_disabled_until = NULL,
                auto_disabled_last_error = NULL,
                revision = revision + 1,
                updated_at = now()
            WHERE id = $1 AND deleted_at IS NULL
            RETURNING id, name, base_url, api_key, auth_type, enabled, priority,
                      max_concurrent_requests, usage_projection_mode, stream_response_mode, raw_usage_source, model_filter_mode,
                      auto_disable_policy, pre_output_stream_retry_mode,
                      auto_disabled, auto_disabled_reason, auto_disabled_at,
                      auto_disabled_until, auto_disabled_last_error,
                      normalize_model_version_dots, model_mapping_mode,
                      model_mapping_require_match, model_mapping_rules, supported_models,
                      route_mode, route_rules, notes, tags, proxy_url, cost_multiplier, overrides,
                      revision, created_at, updated_at
            "#,
        )
        .bind(id as i64)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| account_from_row(row, mask_secrets))
            .transpose()
    }

    #[cfg(test)]
    pub async fn auto_disable_account(
        &self,
        id: u64,
        reason: &str,
        last_error: &str,
        duration_secs: u64,
    ) -> anyhow::Result<()> {
        let Some(pool) = self.get_account(id, true).await? else {
            return Ok(());
        };
        let _ = self
            .auto_disable_account_if_unchanged(id, pool.revision, reason, last_error, duration_secs)
            .await?;
        Ok(())
    }

    pub async fn auto_disable_account_if_unchanged(
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
            UPDATE accounts
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
                .map_err(|_| anyhow::anyhow!("账号 revision 超出 PgSQL BIGINT 范围"))?,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn list_route_scenarios(&self) -> anyhow::Result<Vec<RouteScenarioRow>> {
        let rows = sqlx::query(
            "SELECT id, name, description, content, revision, created_at, updated_at \
             FROM route_scenarios ORDER BY id ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(route_scenario_from_row).collect()
    }

    pub async fn get_route_scenario(&self, id: u64) -> anyhow::Result<Option<RouteScenarioRow>> {
        let row = sqlx::query(
            "SELECT id, name, description, content, revision, created_at, updated_at \
             FROM route_scenarios WHERE id = $1",
        )
        .bind(id as i64)
        .fetch_optional(&self.pool)
        .await?;
        row.map(route_scenario_from_row).transpose()
    }

    pub async fn insert_route_scenario(
        &self,
        name: &str,
        description: Option<&str>,
        content: &serde_json::Value,
    ) -> anyhow::Result<RouteScenarioRow> {
        let row = sqlx::query(
            "INSERT INTO route_scenarios (name, description, content) VALUES ($1, $2, $3) \
             RETURNING id, name, description, content, revision, created_at, updated_at",
        )
        .bind(name)
        .bind(description)
        .bind(content)
        .fetch_one(&self.pool)
        .await?;
        route_scenario_from_row(row)
    }

    pub async fn update_route_scenario(
        &self,
        id: u64,
        name: &str,
        description: Option<&str>,
        content: &serde_json::Value,
    ) -> anyhow::Result<Option<RouteScenarioRow>> {
        let row = sqlx::query(
            "UPDATE route_scenarios SET name = $2, description = $3, content = $4, \
             revision = revision + 1, updated_at = now() WHERE id = $1 \
             RETURNING id, name, description, content, revision, created_at, updated_at",
        )
        .bind(id as i64)
        .bind(name)
        .bind(description)
        .bind(content)
        .fetch_optional(&self.pool)
        .await?;
        row.map(route_scenario_from_row).transpose()
    }

    pub async fn delete_route_scenario(&self, id: u64) -> anyhow::Result<bool> {
        let result = sqlx::query("DELETE FROM route_scenarios WHERE id = $1")
            .bind(id as i64)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
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
                    source, source_url, last_synced_at, last_error, updated_at,
                    cache_creation_1h_input_token_cost
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, now(), $10)
                ON CONFLICT (model) DO UPDATE
                SET input_cost_per_token = EXCLUDED.input_cost_per_token,
                    output_cost_per_token = EXCLUDED.output_cost_per_token,
                    cache_creation_input_token_cost = EXCLUDED.cache_creation_input_token_cost,
                    cache_read_input_token_cost = EXCLUDED.cache_read_input_token_cost,
                    cache_creation_1h_input_token_cost = EXCLUDED.cache_creation_1h_input_token_cost,
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
            .bind(item.pricing.cache_creation_1h_input_token_cost)
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
                   cache_creation_input_token_cost, cache_read_input_token_cost,
                   cache_creation_1h_input_token_cost
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
                    cache_creation_1h_input_token_cost: row
                        .try_get("cache_creation_1h_input_token_cost")?,
                    cache_read_input_token_cost: row.try_get("cache_read_input_token_cost")?,
                }
                .with_derived_cache_creation_1h_cost(),
            );
        }
        Ok(prices)
    }

    pub async fn load_pricing_status(&self) -> anyhow::Result<Option<PricingStatus>> {
        let rows = sqlx::query(
            r#"
            SELECT model, input_cost_per_token, output_cost_per_token,
                   cache_creation_input_token_cost, cache_read_input_token_cost,
                   cache_creation_1h_input_token_cost,
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
                    cache_creation_1h_input_token_cost: row
                        .try_get("cache_creation_1h_input_token_cost")?,
                    cache_read_input_token_cost: row.try_get("cache_read_input_token_cost")?,
                }
                .with_derived_cache_creation_1h_cost(),
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
                    last_synced_at, last_error, updated_at, listed
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, now(), $11)
                ON CONFLICT (model) DO UPDATE
                SET listed = EXCLUDED.listed, display_name = EXCLUDED.display_name,
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
            .bind(item.listed)
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
                updated_at
            )
            VALUES ('default', $1, $2, $3, $4, now())
            ON CONFLICT (id) DO UPDATE
            SET source = EXCLUDED.source,
                last_synced_at = EXCLUDED.last_synced_at,
                last_error = EXCLUDED.last_error,
                model_count = EXCLUDED.model_count,
                updated_at = now()
            "#,
        )
        .bind(&status.source)
        .bind(&status.last_synced_at)
        .bind(&status.last_error)
        .bind(status.model_count as i32)
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
                   last_synced_at, last_error, listed
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
            SELECT source, last_synced_at, last_error
            FROM model_capabilities_sync_status
            WHERE id = 'default'
            "#,
        )
        .fetch_optional(&self.pool)
        .await?;
        let (source, last_synced_at, last_error) = if let Some(row) = status_row {
            (
                row.try_get("source")?,
                row.try_get("last_synced_at")?,
                row.try_get("last_error")?,
            )
        } else {
            (
                rows[0].try_get("source")?,
                rows[0].try_get("last_synced_at")?,
                rows[0].try_get("last_error")?,
            )
        };
        let mut models = Vec::with_capacity(rows.len());
        for row in rows {
            models.push(ModelCapabilityItem {
                listed: row.try_get("listed")?,
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
        }))
    }

    pub async fn promote_account_models(&self, models: &[String]) -> anyhow::Result<Vec<String>> {
        let mut tx = self.pool.begin().await?;
        let mut promoted = Vec::new();
        for model in models {
            let Some(model) = crate::account::normalize_catalog_model(model) else {
                continue;
            };
            let inserted = sqlx::query("INSERT INTO model_capabilities (model, display_name, max_input_tokens, max_output_tokens, supports_prompt_caching, supported_input_types, source, listed) VALUES ($1, $1, 200000, 64000, true, ARRAY['TEXT'], 'account_discovery', false) ON CONFLICT (model) DO NOTHING RETURNING model").bind(&model).fetch_optional(&mut *tx).await?;
            if inserted.is_some() {
                promoted.push(model);
            }
        }
        tx.commit().await?;
        Ok(promoted)
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
                last_synced_at, last_error, updated_at, listed
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NULL, NULL, now(), $9)
            ON CONFLICT (model) DO UPDATE
            SET listed = EXCLUDED.listed, display_name = EXCLUDED.display_name,
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
        .bind(item.listed)
        .execute(&mut *tx)
        .await?;

        if let Some(pricing) = pricing {
            sqlx::query(
                r#"
                INSERT INTO model_pricing (
                    model, input_cost_per_token, output_cost_per_token,
                    cache_creation_input_token_cost, cache_read_input_token_cost,
                    source, source_url, last_synced_at, last_error, updated_at,
                    cache_creation_1h_input_token_cost
                )
                VALUES ($1, $2, $3, $4, $5, $6, 'manual', NULL, NULL, now(), $7)
                ON CONFLICT (model) DO UPDATE
                SET input_cost_per_token = EXCLUDED.input_cost_per_token,
                    output_cost_per_token = EXCLUDED.output_cost_per_token,
                    cache_creation_input_token_cost = EXCLUDED.cache_creation_input_token_cost,
                    cache_read_input_token_cost = EXCLUDED.cache_read_input_token_cost,
                    cache_creation_1h_input_token_cost = EXCLUDED.cache_creation_1h_input_token_cost,
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
            .bind(pricing.cache_creation_1h_input_token_cost)
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

    /// 启动对齐种子时使用：删掉被判定为过期的种子派生条目，再 upsert 全部条目。
    pub async fn replace_seed_model_capabilities(
        &self,
        status: &ModelCapabilitiesStatus,
        removed: &[String],
    ) -> anyhow::Result<()> {
        if !removed.is_empty() {
            sqlx::query(
                "DELETE FROM model_capabilities WHERE source = $1 AND model = ANY($2::text[])",
            )
            .bind(crate::anthropic::model_capabilities::SEED_SOURCE)
            .bind(removed)
            .execute(&self.pool)
            .await?;
        }
        self.save_model_capabilities_status(status).await
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

    /// Audit page query. Every filter is optional and applied in SQL so paging stays exact.
    pub async fn query_admin_audit_logs_filtered(
        &self,
        page: usize,
        limit: usize,
        filter: &AdminAuditLogFilter,
    ) -> anyhow::Result<AdminAuditLogPage> {
        let page = page.max(1);
        let limit = if limit == 0 { 20 } else { limit.min(1000) };
        let offset = page.saturating_sub(1).saturating_mul(limit);
        let mut builder = QueryBuilder::<Postgres>::new(
            "SELECT id, created_at, actor, action, object_type, object_id, success, error_message, detail FROM admin_audit_logs WHERE TRUE",
        );
        push_admin_audit_log_filter(&mut builder, filter);
        builder.push(" ORDER BY created_at DESC, id DESC OFFSET ");
        builder.push_bind(usize_to_i64(offset));
        builder.push(" LIMIT ");
        builder.push_bind(usize_to_i64(limit.saturating_add(1)));
        let rows = builder.build().fetch_all(&self.pool).await?;
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

    /// Account change counts per event kind since `since`, plus how many distinct accounts.
    /// 账号异动按类型计数：`[since, until)`（`until` 缺省不设上限），可按账号过滤。
    pub async fn summarize_account_events(
        &self,
        since: DateTime<Utc>,
        until: Option<DateTime<Utc>>,
        source: Option<&str>,
        account_id: Option<u64>,
    ) -> anyhow::Result<Vec<AccountEventSummaryRow>> {
        let rows = sqlx::query(
            r#"
            SELECT detail->>'accountEvent' AS event,
                   COUNT(*) AS events,
                   COUNT(DISTINCT (object_type, object_id)) AS accounts
            FROM admin_audit_logs
            WHERE detail ? 'accountEvent'
              AND created_at >= $1
              AND ($3::timestamptz IS NULL OR created_at < $3)
              AND ($2::text IS NULL OR detail->>'accountSource' = $2)
              AND ($4::text IS NULL OR object_id = $4)
            GROUP BY detail->>'accountEvent'
            "#,
        )
        .bind(since)
        .bind(source)
        .bind(until)
        .bind(account_id.map(|id| id.to_string()))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(AccountEventSummaryRow {
                    event: row.try_get("event")?,
                    events: row.try_get::<i64, _>("events")?.max(0) as u64,
                    accounts: row.try_get::<i64, _>("accounts")?.max(0) as u64,
                })
            })
            .collect()
    }

    /// Deletes up to `limit` audit rows older than `retention`, oldest first. Small batches with
    /// short lock and statement timeouts keep it from holding locks the request path needs;
    /// rows another session is touching are skipped and picked up next time.
    pub async fn delete_expired_admin_audit_logs(
        &self,
        retention: std::time::Duration,
        limit: usize,
    ) -> anyhow::Result<u64> {
        let limit = limit.clamp(1, ADMIN_AUDIT_CLEANUP_MAX_BATCH);
        let retention_secs = i64::try_from(retention.as_secs())
            .map_err(|_| anyhow::anyhow!("审计日志保留时长超出 PgSQL 范围"))?;
        let mut tx = self.pool.begin().await?;
        sqlx::query(&format!(
            "SET LOCAL lock_timeout = '{}ms'",
            ADMIN_AUDIT_CLEANUP_LOCK_TIMEOUT_MS
        ))
        .execute(&mut *tx)
        .await?;
        sqlx::query(&format!(
            "SET LOCAL statement_timeout = '{}ms'",
            ADMIN_AUDIT_CLEANUP_STATEMENT_TIMEOUT_MS
        ))
        .execute(&mut *tx)
        .await?;
        let removed = sqlx::query(
            r#"
            WITH expired AS (
                SELECT id
                FROM admin_audit_logs
                WHERE created_at < now() - ($1::bigint * interval '1 second')
                ORDER BY created_at ASC, id ASC
                LIMIT $2
                FOR UPDATE SKIP LOCKED
            )
            DELETE FROM admin_audit_logs logs
            USING expired
            WHERE logs.id = expired.id
            "#,
        )
        .bind(retention_secs)
        .bind(usize_to_i64(limit))
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.commit().await?;
        Ok(removed)
    }

    /// Usage of one request API key from the usage rollups, in the units key limits use.
    /// `since = None` reads the lifetime totals. Rollups are written shortly after each request,
    /// so the most recent seconds may be missing.
    pub async fn request_key_usage_since(
        &self,
        key_id: &str,
        since: Option<DateTime<Utc>>,
    ) -> anyhow::Result<RequestKeyUsageTotals> {
        let row = match since {
            Some(since) => sqlx::query(
                r#"
                SELECT COALESCE(SUM(total_estimated_cost_usd), 0)::float8 AS cost_usd,
                       COALESCE(SUM(account_cost_usd), 0)::float8 AS credits,
                       COALESCE(SUM(billable_input_tokens + total_cache_read_input_tokens + total_output_tokens), 0)::float8 AS tokens
                FROM usage_rollup_time_buckets
                WHERE dimension = 'request_api_key' AND dimension_key = $1 AND bucket_start >= $2
                "#,
            )
            .bind(key_id)
            .bind(since)
            .fetch_one(&self.pool)
            .await?,
            None => sqlx::query(
                r#"
                SELECT COALESCE(SUM(total_estimated_cost_usd), 0)::float8 AS cost_usd,
                       COALESCE(SUM(account_cost_usd), 0)::float8 AS credits,
                       COALESCE(SUM(billable_input_tokens + total_cache_read_input_tokens + total_output_tokens), 0)::float8 AS tokens
                FROM usage_rollup_totals
                WHERE dimension = 'request_api_key' AND dimension_key = $1
                "#,
            )
            .bind(key_id)
            .fetch_one(&self.pool)
            .await?,
        };
        Ok(RequestKeyUsageTotals {
            cost_usd: row.try_get("cost_usd")?,
            credits: row.try_get("credits")?,
            tokens: row.try_get("tokens")?,
        })
    }

    /// 每个请求密钥在 `since` 之后被准入拒绝的次数（限额、并发等），来自
    /// `request_api_key_rejection` 维度的小时桶。返回 `key_id -> 次数`。
    pub async fn request_key_rejections_since(
        &self,
        since: DateTime<Utc>,
    ) -> anyhow::Result<HashMap<String, u64>> {
        let rows = sqlx::query(
            r#"
            SELECT dimension_key, COALESCE(SUM(error_requests), 0)::bigint AS rejected
            FROM usage_rollup_time_buckets
            WHERE dimension = $1 AND bucket_start >= $2
            GROUP BY dimension_key
            "#,
        )
        .bind(USAGE_ROLLUP_REQUEST_KEY_REJECTION_DIMENSION)
        .bind(usage_rollup_bucket_start(since))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let key: String = row.try_get("dimension_key")?;
                let rejected: i64 = row.try_get("rejected")?;
                Ok((key, rejected.max(0) as u64))
            })
            .collect()
    }

    /// 写一条账号质量采样快照（EWMA 当前值）。调用方按账号每 60 秒节流一次。
    pub async fn record_account_quality_sample(
        &self,
        sample: &AccountQualitySampleRow,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO account_quality_samples (
                account_id, sampled_at, recent_error_rate, ttft_ewma_ms, latency_ewma_ms, sample_count
            )
            VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT (account_id, sampled_at) DO UPDATE SET
                recent_error_rate = EXCLUDED.recent_error_rate,
                ttft_ewma_ms = EXCLUDED.ttft_ewma_ms,
                latency_ewma_ms = EXCLUDED.latency_ewma_ms,
                sample_count = EXCLUDED.sample_count
            "#,
        )
        .bind(u64_to_i64(sample.account_id))
        .bind(sample.sampled_at)
        .bind(sample.recent_error_rate)
        .bind(sample.ttft_ewma_ms)
        .bind(sample.latency_ewma_ms)
        .bind(u64_to_i64(sample.sample_count))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 一个账号最近 `since` 之后的质量采样点，按时间升序。
    /// 质量分历史：`[since, until)` 内按 `bucket_secs` 秒分桶取平均（多实例、每分钟一行的
    /// 原始点在长窗口里会有上万个），按时间升序。桶起点作为点的时间。
    pub async fn account_quality_history(
        &self,
        account_id: u64,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
        bucket_secs: u32,
    ) -> anyhow::Result<Vec<AccountQualitySampleRow>> {
        let rows = sqlx::query(
            r#"
            SELECT
                account_id,
                to_timestamp(floor(extract(epoch FROM sampled_at) / $4) * $4) AS bucket_at,
                AVG(recent_error_rate)::double precision AS recent_error_rate,
                AVG(ttft_ewma_ms)::double precision AS ttft_ewma_ms,
                AVG(latency_ewma_ms)::double precision AS latency_ewma_ms,
                MAX(sample_count)::bigint AS sample_count
            FROM account_quality_samples
            WHERE account_id = $1 AND sampled_at >= $2 AND sampled_at < $3
            GROUP BY account_id, bucket_at
            ORDER BY bucket_at ASC
            LIMIT 5000
            "#,
        )
        .bind(u64_to_i64(account_id))
        .bind(since)
        .bind(until)
        .bind(f64::from(bucket_secs.max(1)))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(AccountQualitySampleRow {
                    account_id: row_i64_to_u64(&row, "account_id")?,
                    sampled_at: row.try_get("bucket_at")?,
                    recent_error_rate: row.try_get("recent_error_rate")?,
                    ttft_ewma_ms: row.try_get("ttft_ewma_ms")?,
                    latency_ewma_ms: row.try_get("latency_ewma_ms")?,
                    sample_count: row_i64_to_u64(&row, "sample_count")?,
                })
            })
            .collect()
    }

    /// 删除早于 `retention` 的质量采样点，最多 `limit` 行；返回删除行数。
    pub async fn delete_expired_account_quality_samples(
        &self,
        retention: std::time::Duration,
        limit: usize,
    ) -> anyhow::Result<u64> {
        let retention_secs = i64::try_from(retention.as_secs())
            .map_err(|_| anyhow::anyhow!("质量采样保留时长超出 PgSQL 范围"))?;
        let cutoff = Utc::now() - ChronoDuration::seconds(retention_secs);
        let result = sqlx::query(
            r#"
            DELETE FROM account_quality_samples
            WHERE (account_id, sampled_at) IN (
                SELECT account_id, sampled_at
                FROM account_quality_samples
                WHERE sampled_at < $1
                ORDER BY sampled_at ASC
                LIMIT $2
            )
            "#,
        )
        .bind(cutoff)
        .bind(usize_to_i64(limit.clamp(1, 10_000)))
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}

/// 一条账号质量采样快照：Redis 里的 EWMA 在某一时刻的值。
#[derive(Debug, Clone, PartialEq)]
pub struct AccountQualitySampleRow {
    pub account_id: u64,
    pub sampled_at: DateTime<Utc>,
    pub recent_error_rate: f64,
    pub ttft_ewma_ms: Option<f64>,
    pub latency_ewma_ms: Option<f64>,
    pub sample_count: u64,
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

/// Scans active rollup-bearing detail rows and rewrites the two overview total columns.
async fn backfill_usage_overview_totals_in_tx(
    tx: &mut Transaction<'_, Postgres>,
) -> anyhow::Result<u64> {
    for table in ["usage_rollup_time_buckets", "usage_rollup_totals"] {
        sqlx::query(&format!(
            "UPDATE {table} SET account_cost_usd = 0, account_raw_tokens = 0 WHERE account_cost_usd <> 0 OR account_raw_tokens <> 0"
        ))
        .execute(&mut **tx)
        .await?;
    }
    let mut backfill = UsageOverviewTotalsBackfill::default();
    let mut scanned = 0_u64;
    let mut last_id: Option<String> = None;
    loop {
        let rows = sqlx::query(
            r#"
            SELECT id, data
            FROM usage_records
            WHERE deleted_at IS NULL
              AND rollup_active
              AND ($1::text IS NULL OR id > $1)
            ORDER BY id ASC
            LIMIT 1000
            "#,
        )
        .bind(last_id.as_deref())
        .fetch_all(&mut **tx)
        .await?;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            last_id = Some(row.try_get("id")?);
            let value: serde_json::Value = row.try_get("data")?;
            let mut record: UsageRecord = serde_json::from_value(value)?;
            apply_usage_record_legacy_cost_compatibility(&mut record);
            backfill.add_record(&record);
            scanned += 1;
        }
    }
    backfill.apply(tx).await?;
    Ok(scanned)
}

/// Marks the overview totals backfill as applied so startup does not repeat it.
async fn mark_usage_overview_totals_backfilled(
    tx: &mut Transaction<'_, Postgres>,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO schema_migrations (version, checksum, applied_at)
        VALUES ($1, $2, now())
        ON CONFLICT (version) DO NOTHING
        "#,
    )
    .bind(USAGE_OVERVIEW_TOTALS_BACKFILL_VERSION)
    .bind(sha256_hex(USAGE_OVERVIEW_TOTALS_BACKFILL_VERSION))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Startup path for the overview totals backfill: bounded like the index creation. Small
/// detail tables are backfilled in place; large ones are skipped with a pointer to the
/// maintenance command so a routine upgrade never blocks on multi-GB usage history.
/// 汇总表里的记录数是否多于现存明细：明细保留任务删明细时保留汇总，所以汇总可能
/// 覆盖了明细里已经没有的历史。这时"清零后从明细重算"会把那部分历史永久抹掉。
/// 返回 `(汇总条数, 现存明细条数)`，前者大于后者即不能从明细重建。
async fn usage_rollups_exceed_live_records(
    tx: &mut Transaction<'_, Postgres>,
) -> anyhow::Result<Option<(i64, i64)>> {
    let row = sqlx::query(
        r#"
        SELECT
            COALESCE((
                SELECT requests FROM usage_rollup_totals
                WHERE dimension = 'global' AND dimension_key = 'all'
            ), 0)::bigint AS summarized,
            (
                SELECT COUNT(*) FROM usage_records
                WHERE deleted_at IS NULL AND rollup_active
            )::bigint AS live
        "#,
    )
    .fetch_one(&mut **tx)
    .await?;
    let summarized: i64 = row.try_get("summarized")?;
    let live: i64 = row.try_get("live")?;
    Ok((summarized > live).then_some((summarized, live)))
}

fn rebuild_refused_after_retention(command: &str, summarized: i64, live: i64) -> anyhow::Error {
    anyhow::anyhow!(
        "{command} 已拒绝执行：汇总表覆盖 {summarized} 条请求，现存明细只有 {live} 条\
         （明细保留任务删掉了更早的明细但保留了汇总）。从明细重建会永久丢失这部分历史统计。"
    )
}

async fn backfill_usage_overview_totals_if_startup_safe(
    tx: &mut Transaction<'_, Postgres>,
) -> anyhow::Result<()> {
    let applied: Option<String> =
        sqlx::query_scalar("SELECT version FROM schema_migrations WHERE version = $1")
            .bind(USAGE_OVERVIEW_TOTALS_BACKFILL_VERSION)
            .fetch_optional(&mut **tx)
            .await?;
    if applied.is_some() {
        return Ok(());
    }
    let size = relation_size_bytes(tx, "usage_records").await?;
    if size > USAGE_OVERVIEW_TOTALS_STARTUP_MAX_BYTES {
        tracing::warn!(
            usage_records_bytes = size,
            max_startup_bytes = USAGE_OVERVIEW_TOTALS_STARTUP_MAX_BYTES,
            "usage_records 过大，启动时跳过总览合计列回填；总览的账号成本/原始 token 在回填前只统计新记录，请低峰期执行 maintenance usage-overview-totals-backfill"
        );
        return Ok(());
    }
    if let Some((summarized, live)) = usage_rollups_exceed_live_records(tx).await? {
        tracing::warn!(
            summarized,
            live,
            "汇总表覆盖的历史多于现存明细，启动时跳过总览合计列回填，避免抹掉已删明细对应的统计"
        );
        mark_usage_overview_totals_backfilled(tx).await?;
        return Ok(());
    }
    let scanned = backfill_usage_overview_totals_in_tx(tx).await?;
    mark_usage_overview_totals_backfilled(tx).await?;
    tracing::info!(scanned, "已从明细回填总览合计列（账号成本、原始 token）");
    Ok(())
}

/// 任何一个版本的小时桶压缩执行过，数据就已经是小时粒度，不再重复压缩。
async fn usage_rollup_hour_compression_applied(
    conn: &mut sqlx::pool::PoolConnection<Postgres>,
) -> anyhow::Result<bool> {
    let applied: Option<String> = sqlx::query_scalar(
        "SELECT version FROM schema_migrations WHERE version = ANY($1::text[]) LIMIT 1",
    )
    .bind(vec![
        "usage-rollup-hour-bucket-compression-v1".to_string(),
        "usage-rollup-hour-bucket-compression-v2".to_string(),
        "usage-rollup-hour-bucket-compression-v3".to_string(),
    ])
    .fetch_optional(&mut **conn)
    .await?;
    Ok(applied.is_some())
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

/// A request API key's usage in limit units, read from the usage rollups.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RequestKeyUsageTotals {
    pub cost_usd: f64,
    pub credits: f64,
    pub tokens: f64,
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

/// Optional filters for the audit page. Empty strings are treated as absent.
#[derive(Debug, Clone, Default)]
pub struct AdminAuditLogFilter {
    /// Only account change events (`detail.accountEvent` present).
    pub account_events_only: bool,
    /// One of the account event kinds, e.g. `quota_exhausted`.
    pub account_event: Option<String>,
    /// `local` or `external`.
    pub account_source: Option<String>,
    /// `credential` / `account` / ...
    pub object_type: Option<String>,
    /// Exact object id.
    pub object_id: Option<String>,
    /// Substring of the account label (email, pool name, ...), case-insensitive.
    pub account_query: Option<String>,
    pub action: Option<String>,
    /// Only these actions (the console maps its categories to action lists).
    pub actions: Option<Vec<String>>,
    /// `admin-api`, `system-scheduler`, ...
    pub actor: Option<String>,
    pub success: Option<bool>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    /// Free text: case-insensitive substring of action, actor, object type, object id,
    /// error message or the account label inside `detail`. Runs server side so it covers
    /// every page, not just the one already loaded.
    pub text_query: Option<String>,
}

fn push_admin_audit_log_filter(
    builder: &mut QueryBuilder<'_, Postgres>,
    filter: &AdminAuditLogFilter,
) {
    fn present(value: &Option<String>) -> Option<String> {
        value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    }
    if filter.account_events_only {
        builder.push(" AND detail ? 'accountEvent'");
    }
    if let Some(event) = present(&filter.account_event) {
        builder
            .push(" AND detail->>'accountEvent' = ")
            .push_bind(event);
    }
    if let Some(source) = present(&filter.account_source) {
        builder
            .push(" AND detail->>'accountSource' = ")
            .push_bind(source);
    }
    if let Some(object_type) = present(&filter.object_type) {
        builder.push(" AND object_type = ").push_bind(object_type);
    }
    if let Some(object_id) = present(&filter.object_id) {
        builder.push(" AND object_id = ").push_bind(object_id);
    }
    if let Some(query) = present(&filter.account_query) {
        let pattern = format!("%{}%", escape_like_pattern(&query));
        builder
            .push(" AND (COALESCE(detail->>'accountLabel', detail->>'credentialLabel', '') ILIKE ")
            .push_bind(pattern.clone())
            .push(" ESCAPE '\\' OR object_id = ")
            .push_bind(query.trim_start_matches('#').to_string())
            .push(")");
    }
    if let Some(action) = present(&filter.action) {
        builder.push(" AND action = ").push_bind(action);
    }
    if let Some(actions) = filter
        .actions
        .as_ref()
        .map(|values| {
            values
                .iter()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|values| !values.is_empty())
    {
        builder
            .push(" AND action = ANY(")
            .push_bind(actions)
            .push(")");
    }
    if let Some(text) = present(&filter.text_query) {
        let pattern = format!("%{}%", escape_like_pattern(&text));
        builder
            .push(" AND (action ILIKE ")
            .push_bind(pattern.clone())
            .push(" ESCAPE '\\' OR actor ILIKE ")
            .push_bind(pattern.clone())
            .push(" ESCAPE '\\' OR object_type ILIKE ")
            .push_bind(pattern.clone())
            .push(" ESCAPE '\\' OR COALESCE(object_id, '') ILIKE ")
            .push_bind(pattern.clone())
            .push(" ESCAPE '\\' OR COALESCE(error_message, '') ILIKE ")
            .push_bind(pattern.clone())
            .push(" ESCAPE '\\' OR COALESCE(detail->>'accountLabel', detail->>'credentialLabel', '') ILIKE ")
            .push_bind(pattern)
            .push(" ESCAPE '\\')");
    }
    if let Some(actor) = present(&filter.actor) {
        builder.push(" AND actor = ").push_bind(actor);
    }
    if let Some(success) = filter.success {
        builder.push(" AND success = ").push_bind(success);
    }
    if let Some(since) = filter.since {
        builder.push(" AND created_at >= ").push_bind(since);
    }
    if let Some(until) = filter.until {
        builder.push(" AND created_at < ").push_bind(until);
    }
}

fn escape_like_pattern(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountEventSummaryRow {
    pub event: String,
    pub events: u64,
    pub accounts: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountEventSummaryResponse {
    pub hours: u32,
    pub since: String,
    /// 窗口终点。
    pub until: String,
    pub account_source: Option<String>,
    pub items: Vec<AccountEventSummaryRow>,
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
    pub include_summary: bool,
    pub cutoff_at: DateTime<Utc>,
    pub batch_size: usize,
    pub max_batches: usize,
    pub max_rows: Option<u64>,
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
    pub include_summary: bool,
    pub cutoff_at: DateTime<Utc>,
    pub batch_size: usize,
    pub max_batches: usize,
    pub max_rows: Option<u64>,
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
                job_id, mode, include_summary, cutoff_at, batch_size, max_batches, max_rows,
                pause_ms_between_batches, status, phase, started_at, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'queued', 'postgres', now(), now())
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(job.job_id)
        .bind(job.mode)
        .bind(job.include_summary)
        .bind(job.cutoff_at)
        .bind(usize_to_i64(job.batch_size))
        .bind(usize_to_i64(job.max_batches))
        .bind(job.max_rows.map(u64_to_i64))
        .bind(u64_to_i64(job.pause_ms_between_batches))
        .execute(self.store.pool())
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn latest_cleanup_job(&self) -> anyhow::Result<Option<UsageCleanupJobRow>> {
        let row = sqlx::query(
            r#"
            SELECT job_id, mode, include_summary, cutoff_at, batch_size, max_batches, max_rows,
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
            SELECT job_id, mode, include_summary, cutoff_at, batch_size, max_batches, max_rows,
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
            SELECT job_id, mode, include_summary, cutoff_at, batch_size, max_batches, max_rows,
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
            RETURNING job_id, mode, include_summary, cutoff_at, batch_size, max_batches, max_rows,
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
            RETURNING job_id, mode, include_summary, cutoff_at, batch_size, max_batches, max_rows,
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
            RETURNING job_id, mode, include_summary, cutoff_at, batch_size, max_batches, max_rows,
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

    /// Rollup-first overview split for the redesigned admin dashboard.
    ///
    /// The query intentionally reads only complete UTC hour buckets. Local usage
    /// is derived as global minus account rows so the response can expose
    /// an auditable all = local + external reconciliation without scanning the
    /// request-level table.
    pub async fn dashboard_overview(
        &self,
        spec: &UsageOverviewSpec,
    ) -> anyhow::Result<UsageOverviewResponse> {
        let dimensions = vec![
            "global".to_string(),
            "account".to_string(),
            USAGE_ROLLUP_ACCOUNT_ATTEMPT_DIMENSION.to_string(),
            "model".to_string(),
            "endpoint".to_string(),
            "request_api_key".to_string(),
            "error".to_string(),
        ];
        let mut tx = self.store.pool().begin().await?;
        configure_usage_dashboard_read_transaction(&mut tx).await?;
        let rows = sqlx::query(
            r#"
            SELECT
                bucket_start,
                dimension,
                dimension_key,
                NULLIF(BTRIM(dimension_label), '') AS dimension_label,
                requests,
                success_requests,
                error_requests,
                total_input_tokens,
                total_compat_input_tokens,
                total_output_tokens,
                total_cache_read_input_tokens,
                total_cache_creation_input_tokens,
                total_estimated_cost_usd,
                CASE
                    WHEN COALESCE(total_original_cost_usd, 0) <> 0
                    THEN total_original_cost_usd
                    ELSE COALESCE(total_estimated_cost_usd, 0)
                END::double precision AS total_original_cost_usd,

                priced_requests,
                unpriced_requests,
                duration_ms_sum,
                duration_ms_count,
                account_cost_floor_applied_requests,
                account_raw_cost_usd,
                account_shaped_cost_usd,
                account_uplifted_cost_usd,
                account_reported_cost_usd,
                account_billable_cost_usd,
                account_profit_usd,
                account_cost_floor_delta_usd,
                account_cost_usd,
                account_raw_tokens
            FROM usage_rollup_time_buckets
            WHERE dimension = ANY($1::text[])
              AND bucket_start >= $2
              AND bucket_start < $3
            ORDER BY bucket_start, dimension, dimension_key
            "#,
        )
        .bind(&dimensions)
        .bind(spec.from)
        .bind(spec.to)
        .fetch_all(&mut *tx)
        .await?;

        tx.commit().await?;

        let mut all = OverviewAccumulator::default();
        let mut external = OverviewAccumulator::default();
        let mut series = HashMap::<String, OverviewSeriesAccumulator>::new();
        let mut series_groups = OverviewSeriesGroupsAccumulator::default();
        let mut pools = HashMap::<String, OverviewPoolAccumulator>::new();
        let mut models = HashMap::<String, OverviewRankAccumulator>::new();
        let mut keys = HashMap::<String, OverviewRankAccumulator>::new();
        let mut paths = HashMap::<String, OverviewRankAccumulator>::new();
        let mut errors = HashMap::<String, OverviewRankAccumulator>::new();
        // 路由前缀 -> 指标与成本；入口集合单独记，给表格显示该路由下有哪些路径。
        let mut routes = HashMap::<String, OverviewPoolAccumulator>::new();
        let mut route_endpoints = HashMap::<String, BTreeSet<String>>::new();

        for row in rows {
            let bucket_start: DateTime<Utc> = row.try_get("bucket_start")?;
            let dimension: String = row.try_get("dimension")?;
            let dimension_key: String = row.try_get("dimension_key")?;
            let dimension_label: Option<String> = row.try_get("dimension_label")?;
            let metrics = OverviewAccumulator::from_row(&row)?;
            let series_key = overview_series_key(spec, bucket_start);

            match dimension.as_str() {
                "global" if dimension_key == "all" => {
                    all.add(metrics);
                    series
                        .entry(series_key)
                        .or_default()
                        .all
                        .add(metrics, OverviewExternalCosts::from_row(&row)?);
                }
                "account" => {
                    external.add(metrics);
                    let costs = OverviewExternalCosts::from_row(&row)?;
                    let bucket = series.entry(series_key.clone()).or_default();
                    bucket.external.add(metrics, costs);
                    bucket
                        .accounts
                        .entry(dimension_key.clone())
                        .or_default()
                        .add(metrics, costs);
                    series_groups.accounts.add(
                        &dimension_key,
                        dimension_label.as_deref(),
                        costs.raw_cost_usd,
                        metrics.requests,
                    );
                    let pool = pools.entry(dimension_key.clone()).or_default();
                    pool.metrics.add(metrics);
                    pool.account_cost_usd += row.try_get::<f64, _>("account_cost_usd")?;
                    pool.raw_cost_usd += row.try_get::<f64, _>("account_raw_cost_usd")?;
                    pool.shaped_cost_usd += row.try_get::<f64, _>("account_shaped_cost_usd")?;
                    pool.uplifted_cost_usd += row.try_get::<f64, _>("account_uplifted_cost_usd")?;
                    pool.reported_cost_usd += row.try_get::<f64, _>("account_reported_cost_usd")?;
                    pool.billable_cost_usd += row.try_get::<f64, _>("account_billable_cost_usd")?;
                    pool.profit_usd += row.try_get::<f64, _>("account_profit_usd")?;
                    pool.cost_floor_delta_usd +=
                        row.try_get::<f64, _>("account_cost_floor_delta_usd")?;
                    pool.cost_floor_applied_requests =
                        pool.cost_floor_applied_requests.saturating_add(
                            row.try_get::<i64, _>("account_cost_floor_applied_requests")?
                                .max(0) as usize,
                        );
                    if let Some(label) = dimension_label.as_deref().filter(|v| !v.is_empty()) {
                        pool.label = Some(label.to_string());
                    }
                }
                // 尝试口径：同一账号所有尝试（含被切走的失败），只用请求/错误/耗时三组列。
                USAGE_ROLLUP_ACCOUNT_ATTEMPT_DIMENSION => {
                    let pool = pools.entry(dimension_key.clone()).or_default();
                    pool.attempts.add(metrics);
                    if pool.label.is_none()
                        && let Some(label) = dimension_label.as_deref().filter(|v| !v.is_empty())
                    {
                        pool.label = Some(label.to_string());
                    }
                }

                "model" => {
                    let rank = models.entry(dimension_key.clone()).or_default();
                    rank.add(metrics);
                    if let Some(label) = dimension_label.as_deref().filter(|v| !v.is_empty()) {
                        rank.label = label.to_string();
                    }
                }
                "endpoint" => {
                    let rank = paths.entry(dimension_key.clone()).or_default();
                    rank.add(metrics);
                    if let Some(label) = dimension_label.as_deref().filter(|v| !v.is_empty()) {
                        rank.label = label.to_string();
                    }
                    // 每条记录都带完整计费列，所以入口维度按前缀合并就是路由口径，
                    // 不需要新维度，也不需要回填历史。
                    let route = usage_route_prefix_for_endpoint(&dimension_key);
                    let costs = OverviewExternalCosts::from_row(&row)?;
                    series
                        .entry(series_key)
                        .or_default()
                        .routes
                        .entry(route.clone())
                        .or_default()
                        .add(metrics, costs);
                    series_groups.routes.add(
                        &route,
                        None,
                        costs.billable_cost_usd,
                        metrics.requests,
                    );
                    route_endpoints
                        .entry(route.clone())
                        .or_default()
                        .insert(dimension_key.clone());
                    let entry = routes.entry(route).or_default();
                    entry.metrics.add(metrics);
                    entry.account_cost_usd += row.try_get::<f64, _>("account_cost_usd")?;
                    entry.raw_cost_usd += row.try_get::<f64, _>("account_raw_cost_usd")?;
                    entry.shaped_cost_usd += row.try_get::<f64, _>("account_shaped_cost_usd")?;
                    entry.uplifted_cost_usd +=
                        row.try_get::<f64, _>("account_uplifted_cost_usd")?;
                    entry.reported_cost_usd +=
                        row.try_get::<f64, _>("account_reported_cost_usd")?;
                    entry.billable_cost_usd +=
                        row.try_get::<f64, _>("account_billable_cost_usd")?;
                    entry.profit_usd += row.try_get::<f64, _>("account_profit_usd")?;
                    entry.cost_floor_delta_usd +=
                        row.try_get::<f64, _>("account_cost_floor_delta_usd")?;
                    entry.cost_floor_applied_requests =
                        entry.cost_floor_applied_requests.saturating_add(
                            row.try_get::<i64, _>("account_cost_floor_applied_requests")?
                                .max(0) as usize,
                        );
                }
                "request_api_key" => {
                    let rank = keys.entry(dimension_key.clone()).or_default();
                    rank.add(metrics);
                    if let Some(label) = dimension_label.as_deref().filter(|v| !v.is_empty()) {
                        rank.label = label.to_string();
                    }
                }
                "error" => {
                    let rank = errors.entry(dimension_key.clone()).or_default();
                    rank.add(metrics);
                    if let Some(label) = dimension_label.as_deref().filter(|v| !v.is_empty()) {
                        rank.label = label.to_string();
                    }
                }
                _ => {}
            }
        }

        let series_groups = series_groups.finish();
        let series_points = overview_series_points(spec, series, &series_groups);
        let external_metrics = external.to_metrics();

        let mut route_rows = routes
            .into_iter()
            .map(|(route, value)| UsageOverviewRouteRow {
                endpoints: route_endpoints
                    .remove(&route)
                    .map(|set| set.into_iter().collect())
                    .unwrap_or_default(),
                route,
                metrics: value.metrics.to_metrics(),
                raw_cost_usd: value.raw_cost_usd,
                shaped_cost_usd: value.shaped_cost_usd,
                uplifted_cost_usd: value.uplifted_cost_usd,
                reported_cost_usd: value.reported_cost_usd,
                billable_cost_usd: value.billable_cost_usd,
                profit_usd: value.profit_usd,
                cost_floor_delta_usd: value.cost_floor_delta_usd,
                cost_floor_applied_requests: value.cost_floor_applied_requests,
                account_cost_usd: value.account_cost_usd.max(0.0),
            })
            .collect::<Vec<_>>();
        route_rows.sort_by(|left, right| {
            right
                .billable_cost_usd
                .total_cmp(&left.billable_cost_usd)
                .then_with(|| right.metrics.requests.cmp(&left.metrics.requests))
                .then_with(|| left.route.cmp(&right.route))
        });

        let cost_floor_applied_requests = pools
            .values()
            .map(|pool| pool.cost_floor_applied_requests)
            .sum::<usize>();
        let mut pool_rows = pools
            .into_iter()
            .filter_map(|(pool_id, pool)| {
                let Ok(pool_id) = pool_id.parse::<u64>() else {
                    return None;
                };
                let attempts = pool.attempts.to_metrics();
                Some(UsageOverviewPoolRow {
                    pool_id,
                    pool_name: pool.label.unwrap_or_else(|| format!("#{}", pool_id)),
                    metrics: pool.metrics.to_metrics(),
                    raw_cost_usd: pool.raw_cost_usd,
                    shaped_cost_usd: pool.shaped_cost_usd,
                    uplifted_cost_usd: pool.uplifted_cost_usd,
                    reported_cost_usd: pool.reported_cost_usd,
                    billable_cost_usd: pool.billable_cost_usd,
                    profit_usd: pool.profit_usd,
                    cost_floor_delta_usd: pool.cost_floor_delta_usd,
                    cost_floor_applied_requests: pool.cost_floor_applied_requests,
                    account_cost_usd: pool.account_cost_usd.max(0.0),
                    attempt_requests: attempts.requests,
                    attempt_error_requests: attempts.error_requests,
                    attempt_error_rate: attempts.error_rate,
                    attempt_average_duration_ms: attempts.average_duration_ms,
                })
            })
            .collect::<Vec<_>>();
        pool_rows.sort_by(|left, right| {
            right
                .billable_cost_usd
                .total_cmp(&left.billable_cost_usd)
                .then_with(|| right.metrics.requests.cmp(&left.metrics.requests))
                .then_with(|| left.pool_id.cmp(&right.pool_id))
        });

        // 排行里的“账号”就是账号池本身：按下游估算计费排序，和其它维度口径一致。
        let mut top_accounts = pool_rows
            .iter()
            .filter(|pool| pool.metrics.requests > 0)
            .map(|pool| UsageOverviewRankRow {
                key: pool.pool_id.to_string(),
                label: pool.pool_name.clone(),
                requests: pool.metrics.requests,
                error_requests: pool.metrics.error_requests,
                estimated_cost_usd: pool.metrics.estimated_cost_usd,
                success_requests: pool.metrics.success_requests,
                cache_read_input_tokens: pool.metrics.cache_read_input_tokens,
                cache_creation_input_tokens: pool.metrics.cache_creation_input_tokens,
                uncached_input_tokens: pool.metrics.uncached_input_tokens,
                original_cost_usd: pool.metrics.original_cost_usd,
                account_cost_usd: pool.account_cost_usd.max(0.0),
                average_duration_ms: pool.metrics.average_duration_ms,
            })
            .collect::<Vec<_>>();
        top_accounts.sort_by(|left, right| {
            right
                .estimated_cost_usd
                .total_cmp(&left.estimated_cost_usd)
                .then_with(|| right.requests.cmp(&left.requests))
                .then_with(|| left.key.cmp(&right.key))
        });
        top_accounts.truncate(spec.top_n);

        let mut top_models = ranked_overview_rows(models);
        top_models.truncate(spec.top_n);
        let mut top_keys = ranked_overview_rows(keys);
        top_keys.truncate(spec.top_n);
        let mut top_paths = ranked_overview_rows(paths);
        top_paths.truncate(spec.top_n);
        let mut top_errors = ranked_overview_rows(errors);
        top_errors.sort_by(|left, right| {
            right
                .error_requests
                .cmp(&left.error_requests)
                .then_with(|| right.requests.cmp(&left.requests))
                .then_with(|| left.key.cmp(&right.key))
        });
        top_errors.truncate(spec.top_n);

        Ok(UsageOverviewResponse {
            generated_at: Utc::now().to_rfc3339(),
            timezone: spec.timezone.clone(),
            range: crate::anthropic::usage::UsageOverviewRange {
                key: spec.range_key.clone(),
                requested_from: spec.requested_from.to_rfc3339(),
                requested_to: spec.requested_to.to_rfc3339(),
                from: spec.from.to_rfc3339(),
                to: spec.to.to_rfc3339(),
                data_through: spec.data_through.to_rfc3339(),
                granularity: spec.granularity.clone(),
            },
            totals: UsageOverviewTotals {
                all: all.to_metrics(),
                external: external_metrics,
            },
            external: UsageOverviewExternal {
                raw_cost_usd: pool_rows.iter().map(|pool| pool.raw_cost_usd).sum(),
                shaped_cost_usd: pool_rows.iter().map(|pool| pool.shaped_cost_usd).sum(),
                uplifted_cost_usd: pool_rows.iter().map(|pool| pool.uplifted_cost_usd).sum(),
                reported_cost_usd: pool_rows.iter().map(|pool| pool.reported_cost_usd).sum(),
                billable_cost_usd: pool_rows.iter().map(|pool| pool.billable_cost_usd).sum(),
                profit_usd: pool_rows.iter().map(|pool| pool.profit_usd).sum(),
                cost_floor_delta_usd: pool_rows.iter().map(|pool| pool.cost_floor_delta_usd).sum(),
                cost_floor_applied_requests,
                account_cost_usd: all.account_cost_usd.max(0.0),
                raw_tokens: all.account_raw_tokens.max(0),
                pools: pool_rows,
            },
            routes: route_rows,
            series: series_points,
            series_groups,
            top_models,
            top_accounts,
            top_keys,
            top_paths,
            top_errors,
        })
    }

    /// 按账号的总耗时与首字延迟 p50/p95/p99。直接扫 `usage_records` 明细
    /// （`percentile_cont`），所以调用方把范围限制在 `USAGE_OVERVIEW_HOURLY_MAX_DAYS` 内。
    /// 账号取 `data->>'accountId'`（明细表的 `account_id` 列沿用 main 存的是凭据 id），
    /// 与风控页同一口径，走 `idx_usage_records_account_json_created`。
    pub async fn account_latency(
        &self,
        spec: &UsageOverviewSpec,
        account_id: Option<u64>,
    ) -> anyhow::Result<UsageAccountLatencyResponse> {
        let mut tx = self.store.pool().begin().await?;
        configure_usage_dashboard_read_transaction(&mut tx).await?;
        let rows = sqlx::query(
            r#"
            WITH scoped AS (
                SELECT
                    (data->>'accountId')::bigint AS account_id,
                    NULLIF(BTRIM(data->>'accountName'), '') AS account_label,
                    duration_ms::double precision AS duration_ms,
                    CASE
                        WHEN data->>'firstTokenLatencyMs' ~ '^[0-9]+$'
                        THEN (data->>'firstTokenLatencyMs')::double precision
                        ELSE NULL
                    END AS first_token_latency_ms,
                    created_at
                FROM usage_records
                WHERE deleted_at IS NULL
                  AND data->>'accountId' ~ '^[0-9]+$'
                  AND created_at >= $1
                  AND created_at < $2
                  AND ($3::text IS NULL OR data->>'accountId' = $3)
            )
            SELECT
                account_id,
                (ARRAY_AGG(account_label ORDER BY created_at DESC) FILTER (WHERE account_label IS NOT NULL))[1]
                    AS account_label,
                COUNT(*)::bigint AS samples,
                COALESCE(percentile_cont(0.5) WITHIN GROUP (ORDER BY duration_ms), 0)::double precision AS p50_duration_ms,
                COALESCE(percentile_cont(0.95) WITHIN GROUP (ORDER BY duration_ms), 0)::double precision AS p95_duration_ms,
                COALESCE(percentile_cont(0.99) WITHIN GROUP (ORDER BY duration_ms), 0)::double precision AS p99_duration_ms,
                COUNT(first_token_latency_ms)::bigint AS first_token_samples,
                percentile_cont(0.5) WITHIN GROUP (ORDER BY first_token_latency_ms)::double precision AS p50_first_token_latency_ms,
                percentile_cont(0.95) WITHIN GROUP (ORDER BY first_token_latency_ms)::double precision AS p95_first_token_latency_ms,
                percentile_cont(0.99) WITHIN GROUP (ORDER BY first_token_latency_ms)::double precision AS p99_first_token_latency_ms
            FROM scoped
            GROUP BY account_id
            ORDER BY samples DESC, account_id ASC
            "#,
        )
        .bind(spec.from)
        .bind(spec.to)
        .bind(account_id.map(|id| id.to_string()))
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;

        let accounts = rows
            .into_iter()
            .map(|row| {
                let account_id = row_i64_to_u64(&row, "account_id")?;
                let account_label: Option<String> = row.try_get("account_label")?;
                Ok(UsageAccountLatencyRow {
                    account_id,
                    account_name: account_label.unwrap_or_else(|| format!("#{account_id}")),
                    samples: row_i64_to_usize(&row, "samples")?,
                    p50_duration_ms: row.try_get("p50_duration_ms")?,
                    p95_duration_ms: row.try_get("p95_duration_ms")?,
                    p99_duration_ms: row.try_get("p99_duration_ms")?,
                    first_token_samples: row_i64_to_usize(&row, "first_token_samples")?,
                    p50_first_token_latency_ms: row.try_get("p50_first_token_latency_ms")?,
                    p95_first_token_latency_ms: row.try_get("p95_first_token_latency_ms")?,
                    p99_first_token_latency_ms: row.try_get("p99_first_token_latency_ms")?,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        Ok(UsageAccountLatencyResponse {
            generated_at: Utc::now().to_rfc3339(),
            timezone: spec.timezone.clone(),
            range: crate::anthropic::usage::UsageOverviewRange {
                key: spec.range_key.clone(),
                requested_from: spec.requested_from.to_rfc3339(),
                requested_to: spec.requested_to.to_rfc3339(),
                from: spec.from.to_rfc3339(),
                to: spec.to.to_rfc3339(),
                data_through: spec.data_through.to_rfc3339(),
                granularity: spec.granularity.clone(),
            },
            accounts,
        })
    }

    /// 按路由前缀的总耗时与首字延迟 p50/p95/p99，扫 `usage_records` 明细，
    /// 调用方同样把范围限制在 `USAGE_OVERVIEW_HOURLY_MAX_DAYS` 内。路由前缀在 SQL 里用
    /// 贪婪匹配取最后一个 `/v1/` 之前的部分，与 `usage_route_prefix_for_endpoint` 同一口径。
    pub async fn route_latency(
        &self,
        spec: &UsageOverviewSpec,
    ) -> anyhow::Result<UsageRouteLatencyResponse> {
        let mut tx = self.store.pool().begin().await?;
        configure_usage_dashboard_read_transaction(&mut tx).await?;
        let rows = sqlx::query(
            r#"
            WITH scoped AS (
                SELECT
                    COALESCE(NULLIF(BTRIM(endpoint), ''), 'unknown') AS endpoint,
                    duration_ms::double precision AS duration_ms,
                    CASE
                        WHEN data->>'firstTokenLatencyMs' ~ '^[0-9]+$'
                        THEN (data->>'firstTokenLatencyMs')::double precision
                        ELSE NULL
                    END AS first_token_latency_ms
                FROM usage_records
                WHERE deleted_at IS NULL
                  AND created_at >= $1
                  AND created_at < $2
            ),
            routed AS (
                SELECT
                    CASE
                        WHEN endpoint ~ '/v1(/|$)'
                        THEN regexp_replace(endpoint, '^(.*)/v1(/.*)?$', '\1')
                        ELSE endpoint
                    END AS route,
                    duration_ms,
                    first_token_latency_ms
                FROM scoped
            )
            SELECT
                route,
                COUNT(*)::bigint AS samples,
                COALESCE(percentile_cont(0.5) WITHIN GROUP (ORDER BY duration_ms), 0)::double precision AS p50_duration_ms,
                COALESCE(percentile_cont(0.95) WITHIN GROUP (ORDER BY duration_ms), 0)::double precision AS p95_duration_ms,
                COALESCE(percentile_cont(0.99) WITHIN GROUP (ORDER BY duration_ms), 0)::double precision AS p99_duration_ms,
                COUNT(first_token_latency_ms)::bigint AS first_token_samples,
                percentile_cont(0.5) WITHIN GROUP (ORDER BY first_token_latency_ms)::double precision AS p50_first_token_latency_ms,
                percentile_cont(0.95) WITHIN GROUP (ORDER BY first_token_latency_ms)::double precision AS p95_first_token_latency_ms,
                percentile_cont(0.99) WITHIN GROUP (ORDER BY first_token_latency_ms)::double precision AS p99_first_token_latency_ms
            FROM routed
            GROUP BY route
            ORDER BY samples DESC, route ASC
            "#,
        )
        .bind(spec.from)
        .bind(spec.to)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;

        let routes = rows
            .into_iter()
            .map(|row| {
                let route: String = row.try_get("route")?;
                let route = route.trim().trim_end_matches('/').to_string();
                Ok(UsageRouteLatencyRow {
                    route: if route.is_empty() {
                        USAGE_DEFAULT_ROUTE_KEY.to_string()
                    } else {
                        route
                    },
                    samples: row_i64_to_usize(&row, "samples")?,
                    p50_duration_ms: row.try_get("p50_duration_ms")?,
                    p95_duration_ms: row.try_get("p95_duration_ms")?,
                    p99_duration_ms: row.try_get("p99_duration_ms")?,
                    first_token_samples: row_i64_to_usize(&row, "first_token_samples")?,
                    p50_first_token_latency_ms: row.try_get("p50_first_token_latency_ms")?,
                    p95_first_token_latency_ms: row.try_get("p95_first_token_latency_ms")?,
                    p99_first_token_latency_ms: row.try_get("p99_first_token_latency_ms")?,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        Ok(UsageRouteLatencyResponse {
            generated_at: Utc::now().to_rfc3339(),
            timezone: spec.timezone.clone(),
            range: crate::anthropic::usage::UsageOverviewRange {
                key: spec.range_key.clone(),
                requested_from: spec.requested_from.to_rfc3339(),
                requested_to: spec.requested_to.to_rfc3339(),
                from: spec.from.to_rfc3339(),
                to: spec.to.to_rfc3339(),
                data_through: spec.data_through.to_rfc3339(),
                granularity: spec.granularity.clone(),
            },
            routes,
        })
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
            WHERE (
                (deleted_at IS NULL AND created_at < $1)
                OR (deleted_at IS NOT NULL AND deleted_at < $1)
            )
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

    #[cfg(test)]
    pub async fn soft_delete_cleanup_batch(
        &self,
        cutoff: DateTime<Utc>,
        batch_size: usize,
    ) -> anyhow::Result<UsageCleanupBatchResult> {
        self.soft_delete_cleanup_batch_with_summary(cutoff, batch_size, true)
            .await
    }

    /// Soft-delete detail rows while keeping their already-materialized summaries.
    /// This is the path used by the admin cleanup job unless the caller explicitly
    /// asks to clear all historical summaries.
    pub async fn soft_delete_cleanup_batch_preserving_summary(
        &self,
        cutoff: DateTime<Utc>,
        batch_size: usize,
    ) -> anyhow::Result<UsageCleanupBatchResult> {
        self.soft_delete_cleanup_batch_with_summary(cutoff, batch_size, false)
            .await
    }

    async fn soft_delete_cleanup_batch_with_summary(
        &self,
        cutoff: DateTime<Utc>,
        batch_size: usize,
        include_summary: bool,
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
                SET deleted_at = now(),
                    rollup_active = CASE WHEN $3 THEN false ELSE true END,
                    updated_at = now()
                FROM victims AS v
                WHERE u.id = v.id
                RETURNING v.data, v.rollup_active
                "#,
            )
            .bind(cutoff)
            .bind(usize_to_i64(batch_size))
            .bind(include_summary)
            .fetch_all(&mut *tx)
            .await?;
            let processed_rows = victim_rows.len() as u64;
            if include_summary {
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
            }
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

    #[cfg(test)]
    pub async fn hard_delete_cleanup_batch(
        &self,
        cutoff: DateTime<Utc>,
        batch_size: usize,
    ) -> anyhow::Result<UsageCleanupBatchResult> {
        self.hard_delete_cleanup_batch_with_summary(cutoff, batch_size, true)
            .await
    }

    /// Physically delete matching detail rows without touching rollups. This
    /// includes active rows matched by `created_at` and older tombstones matched
    /// by `deleted_at`; the admin cleanup worker uses this default-preserving
    /// path and leaves rollups unchanged.
    pub async fn hard_delete_cleanup_batch_preserving_summary(
        &self,
        cutoff: DateTime<Utc>,
        batch_size: usize,
    ) -> anyhow::Result<UsageCleanupBatchResult> {
        self.hard_delete_cleanup_batch_with_summary(cutoff, batch_size, false)
            .await
    }

    async fn hard_delete_cleanup_batch_with_summary(
        &self,
        cutoff: DateTime<Utc>,
        batch_size: usize,
        include_summary: bool,
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
                    WHERE (
                        (deleted_at IS NULL AND created_at < $1)
                        OR (deleted_at IS NOT NULL AND deleted_at < $1)
                    )
                    ORDER BY COALESCE(deleted_at, created_at) ASC, id ASC
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
            if include_summary {
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
            }
            let has_remaining = if processed_rows < batch_size as u64 {
                Some(
                    sqlx::query_scalar::<_, bool>(
                        r#"
                        SELECT EXISTS (
                            SELECT 1
                            FROM usage_records
                            WHERE (
                                (deleted_at IS NULL AND created_at < $1)
                                OR (deleted_at IS NOT NULL AND deleted_at < $1)
                            )
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
                WHERE (
                    (deleted_at IS NULL AND created_at < $1)
                    OR (deleted_at IS NOT NULL AND deleted_at < $1)
                )
            )
            "#,
        )
        .bind(cutoff)
        .fetch_one(self.store.pool())
        .await
        .map_err(Into::into)
    }

    /// Recreate persisted usage aggregates from the currently visible detail rows.
    ///
    /// Summary-inclusive cleanup first soft-deletes rows older than its cutoff. New
    /// traffic may still be accepted after that cutoff, so a plain TRUNCATE of the
    /// rollup tables would erase fresh rollups and make the live service look empty
    /// until enough new writes accumulate. Rebuilding from active details in the
    /// same reset transaction preserves those post-cutoff records.
    /// Recomputes the overview total columns (`account_cost_usd`, `account_raw_tokens`) of both
    /// rollup tables from active detail rows. Zeroing first keeps the command idempotent;
    /// every other rollup column is left untouched. Runs under the usage cleanup commit
    /// guard so concurrent writers cannot interleave partial sums.
    pub async fn backfill_usage_overview_totals(&self) -> anyhow::Result<u64> {
        let mut tx = self.store.pool().begin().await?;
        acquire_usage_cleanup_commit_guard(&mut tx).await?;
        if let Some((summarized, live)) = usage_rollups_exceed_live_records(&mut tx).await? {
            return Err(rebuild_refused_after_retention(
                "usage-overview-totals-backfill",
                summarized,
                live,
            ));
        }
        let scanned = backfill_usage_overview_totals_in_tx(&mut tx).await?;
        mark_usage_overview_totals_backfilled(&mut tx).await?;
        tx.commit().await?;
        Ok(scanned)
    }

    /// Physically deletes usage detail rows (active rows by `created_at`, tombstones by
    /// `deleted_at`) older than `retention`, oldest first, in one bounded batch. Rollups are
    /// kept, so overview and trends keep their history; only the per-request detail goes.
    /// Returns the number of removed rows; `limit` rows means there may be more.
    pub async fn delete_expired_usage_records(
        &self,
        retention: std::time::Duration,
        limit: usize,
    ) -> anyhow::Result<u64> {
        let limit = limit.clamp(1, USAGE_RETENTION_MAX_BATCH);
        let retention_secs = i64::try_from(retention.as_secs())
            .map_err(|_| anyhow::anyhow!("用量明细保留时长超出 PgSQL 范围"))?;
        let cutoff = Utc::now() - ChronoDuration::seconds(retention_secs);
        let result = self
            .hard_delete_cleanup_batch_preserving_summary(cutoff, limit)
            .await?;
        Ok(result.processed_rows)
    }

    /// Prunes per-conversation rollup rows (`dimension = 'conversation'`, one row per
    /// conversation, never time-bucketed) whose last activity is older than `retention`.
    /// They only feed the "top conversations" list, so dropping stale ones keeps that table
    /// from growing forever once detail retention is on. Bounded by `limit` rows per call.
    pub async fn delete_stale_conversation_rollups(
        &self,
        retention: std::time::Duration,
        limit: usize,
    ) -> anyhow::Result<u64> {
        let limit = limit.clamp(1, USAGE_RETENTION_MAX_BATCH);
        let retention_secs = i64::try_from(retention.as_secs())
            .map_err(|_| anyhow::anyhow!("用量明细保留时长超出 PgSQL 范围"))?;
        let mut tx = self.store.pool().begin().await?;
        configure_usage_cleanup_transaction(&mut tx).await?;
        let removed = sqlx::query(
            r#"
            WITH stale AS (
                SELECT dimension_key
                FROM usage_rollup_totals
                WHERE dimension = 'conversation'
                  AND updated_at < now() - ($1::bigint * interval '1 second')
                ORDER BY updated_at ASC
                LIMIT $2
                FOR UPDATE SKIP LOCKED
            )
            DELETE FROM usage_rollup_totals AS rollup
            USING stale
            WHERE rollup.dimension = 'conversation'
              AND rollup.dimension_key = stale.dimension_key
            "#,
        )
        .bind(retention_secs)
        .bind(usize_to_i64(limit))
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.commit().await?;
        Ok(removed)
    }

    /// 重建上线后新增的汇总维度（账号尝试、密钥准入拒绝）并回填未缓存输入列。
    /// 离线维护命令：先删这些维度的全部行，再从活跃明细重算，重复执行结果一致。
    pub async fn rebuild_usage_route_dimensions(&self) -> anyhow::Result<u64> {
        let dimensions = USAGE_LATE_ADDED_ROLLUP_DIMENSIONS
            .iter()
            .map(|dimension| dimension.to_string())
            .collect::<Vec<_>>();
        let mut tx = self.store.pool().begin().await?;
        acquire_usage_cleanup_commit_guard(&mut tx).await?;
        if let Some((summarized, live)) = usage_rollups_exceed_live_records(&mut tx).await? {
            return Err(rebuild_refused_after_retention(
                "usage-route-dimension-backfill",
                summarized,
                live,
            ));
        }
        for table in ["usage_rollup_time_buckets", "usage_rollup_totals"] {
            sqlx::query(&format!(
                "DELETE FROM {table} WHERE dimension = ANY($1::text[])"
            ))
            .bind(&dimensions)
            .execute(&mut *tx)
            .await?;
        }
        // The uncached-input column was added after launch; recompute it for every
        // dimension from the same expansion the writer uses, leaving the other
        // columns untouched. Zeroing first keeps the command idempotent.
        for table in ["usage_rollup_time_buckets", "usage_rollup_totals"] {
            sqlx::query(&format!(
                "UPDATE {table} SET total_compat_input_tokens = 0 WHERE total_compat_input_tokens <> 0"
            ))
            .execute(&mut *tx)
            .await?;
        }
        let mut compat_input = UsageCompatInputBackfill::default();
        let mut scanned = 0_u64;
        let mut last_id: Option<String> = None;
        loop {
            let rows = sqlx::query(
                r#"
                SELECT id, data
                FROM usage_records
                WHERE deleted_at IS NULL
                  AND rollup_active
                  AND ($1::text IS NULL OR id > $1)
                ORDER BY id ASC
                LIMIT 1000
                "#,
            )
            .bind(last_id.as_deref())
            .fetch_all(&mut *tx)
            .await?;
            if rows.is_empty() {
                break;
            }
            let mut rollups = UsageRollupBatchDelta::default();
            for row in rows {
                last_id = Some(row.try_get("id")?);
                let value: serde_json::Value = row.try_get("data")?;
                let mut record: UsageRecord = serde_json::from_value(value)?;
                apply_usage_record_legacy_cost_compatibility(&mut record);
                rollups.add_record_dimensions(&record, &USAGE_LATE_ADDED_ROLLUP_DIMENSIONS);
                compat_input.add_record(&record);
                scanned += 1;
            }
            rollups.apply(&mut tx).await?;
        }
        compat_input.apply(&mut tx).await?;
        tx.commit().await?;
        Ok(scanned)
    }

    pub async fn reset_usage_rollups_from_active_records(&self) -> anyhow::Result<()> {
        let mut tx = self.store.pool().begin().await?;
        configure_usage_cleanup_transaction(&mut tx).await?;
        acquire_usage_writer_commit_guard(&mut tx).await?;
        for statement in [
            "TRUNCATE TABLE usage_duration_rollup_time_buckets",
            "TRUNCATE TABLE usage_cache_read_rollup_time_buckets",
            "TRUNCATE TABLE usage_cache_read_totals",
            "TRUNCATE TABLE usage_rollup_time_buckets",
            "TRUNCATE TABLE usage_rollup_totals",
        ] {
            sqlx::query(statement).execute(&mut *tx).await?;
        }
        let mut last_id: Option<String> = None;
        loop {
            let rows = sqlx::query(
                r#"
                SELECT id, data
                FROM usage_records
                WHERE deleted_at IS NULL
                  AND rollup_active
                  AND ($1::text IS NULL OR id > $1)
                ORDER BY id ASC
                LIMIT 500
                "#,
            )
            .bind(last_id.as_deref())
            .fetch_all(&mut *tx)
            .await?;
            if rows.is_empty() {
                break;
            }

            let mut rollups = UsageRollupBatchDelta::default();
            for row in rows {
                last_id = Some(row.try_get("id")?);
                let value: serde_json::Value = row.try_get("data")?;
                let mut record: UsageRecord = serde_json::from_value(value)?;
                apply_usage_record_legacy_cost_compatibility(&mut record);
                rollups.add_record(&record, 1);
            }
            rollups.apply(&mut tx).await?;
        }
        tx.commit().await?;
        Ok(())
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


                COALESCE(t.priced_requests, 0)::bigint AS priced_requests,
                COALESCE(t.unpriced_requests, 0)::bigint AS unpriced_requests,
                COALESCE(t.local_prompt_cache_requests, 0)::bigint AS local_prompt_cache_requests,
                COALESCE(t.local_prompt_cache_input_tokens, 0)::bigint AS local_prompt_cache_input_tokens,
                COALESCE(t.local_prompt_cache_read_input_tokens, 0)::bigint AS local_prompt_cache_read_input_tokens,
                COALESCE(t.local_prompt_cache_creation_input_tokens, 0)::bigint AS local_prompt_cache_creation_input_tokens,
                COALESCE(t.simulated_requests, 0)::bigint AS simulated_requests,
                COALESCE(t.upstream_metadata_requests, 0)::bigint AS upstream_metadata_requests,
                COALESCE(t.account_requests, 0)::bigint AS account_requests,
                COALESCE(t.account_priced_requests, 0)::bigint AS account_priced_requests,
                COALESCE(t.account_unpriced_requests, 0)::bigint AS account_unpriced_requests,
                COALESCE(t.account_cost_floor_applied_requests, 0)::bigint AS account_cost_floor_applied_requests,
                COALESCE(t.account_raw_cost_usd, 0)::double precision AS account_raw_cost_usd,
                CASE
                    WHEN COALESCE(t.account_shaped_cost_usd, 0) <> 0
                    THEN t.account_shaped_cost_usd
                    ELSE COALESCE(t.account_reported_cost_usd, 0)
                END::double precision AS account_shaped_cost_usd,
                CASE
                    WHEN COALESCE(t.account_uplifted_cost_usd, 0) <> 0
                    THEN t.account_uplifted_cost_usd
                    ELSE COALESCE(t.account_reported_cost_usd, 0)
                END::double precision AS account_uplifted_cost_usd,
                CASE
                    WHEN COALESCE(t.account_profit_usd, 0) <> 0
                    THEN t.account_profit_usd
                    WHEN COALESCE(t.account_reported_cost_usd, 0) <> 0
                      OR COALESCE(t.account_raw_cost_usd, 0) <> 0
                    THEN COALESCE(t.account_reported_cost_usd, 0) - COALESCE(t.account_raw_cost_usd, 0)
                    ELSE 0
                END::double precision AS account_profit_usd,
                COALESCE(t.account_reported_cost_usd, 0)::double precision AS account_reported_cost_usd,
                COALESCE(t.account_billable_cost_usd, 0)::double precision AS account_billable_cost_usd,
                COALESCE(t.account_cost_floor_delta_usd, 0)::double precision AS account_cost_floor_delta_usd,
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
                     t.priced_requests, t.unpriced_requests,
                     t.local_prompt_cache_requests, t.local_prompt_cache_input_tokens,
                     t.local_prompt_cache_read_input_tokens,
                     t.local_prompt_cache_creation_input_tokens,
                     t.simulated_requests, t.upstream_metadata_requests,
                     t.account_requests, t.account_priced_requests,
                     t.account_unpriced_requests,
                     t.account_cost_floor_applied_requests,
                     t.account_raw_cost_usd, t.account_shaped_cost_usd,
                     t.account_uplifted_cost_usd, t.account_profit_usd,
                     t.account_reported_cost_usd, t.account_billable_cost_usd,
                     t.account_cost_floor_delta_usd
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
            account_billing: UsageAccountBillingSummary {
                requests: row_i64_to_usize(&row, "account_requests")?,
                priced_requests: row_i64_to_usize(&row, "account_priced_requests")?,
                unpriced_requests: row_i64_to_usize(&row, "account_unpriced_requests")?,
                cost_floor_applied_requests: row_i64_to_usize(
                    &row,
                    "account_cost_floor_applied_requests",
                )?,
                raw_cost_usd: row.try_get("account_raw_cost_usd")?,
                shaped_cost_usd: row.try_get("account_shaped_cost_usd")?,
                uplifted_cost_usd: row.try_get("account_uplifted_cost_usd")?,
                profit_usd: row.try_get("account_profit_usd")?,
                reported_cost_usd: row.try_get("account_reported_cost_usd")?,
                billable_cost_usd: row.try_get("account_billable_cost_usd")?,
                cost_floor_delta_usd: row.try_get("account_cost_floor_delta_usd")?,
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
            top_credentials: Vec::new(),
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

    pub async fn dashboard_account_billing_only(
        &self,
        timezone: Option<&str>,
        window_key: &str,
    ) -> anyhow::Result<(String, String, String, Vec<UsageAccountBillingByPool>)> {
        let now = Utc::now();
        let (timezone, offset) = usage_dashboard_timezone(timezone);
        let window_spec = usage_dashboard_window_spec_for_key(now, offset, window_key);
        let specs = [window_spec.clone()];
        let mut billing = self.dashboard_account_billing_by_pool(&specs).await?;
        Ok((
            now.to_rfc3339(),
            timezone,
            window_spec.key.clone(),
            billing.remove(&window_spec.key).unwrap_or_default(),
        ))
    }

    pub async fn account_usage_risk(
        &self,
        query: UsageAccountRiskQuery,
        cost_config: UsageAccountRiskCostConfig,
    ) -> anyhow::Result<UsageAccountRiskResponse> {
        if query.from >= query.to {
            anyhow::bail!("账号 usage 风控查询时间范围无效");
        }

        let cost_target_multiplier = if cost_config.cost_floor_enabled {
            1.0 + (cost_config.cost_floor_margin_percent as f64 / 100.0)
        } else {
            1.0
        }
        .max(1.0);

        let mut tx = self.store.pool().begin().await?;
        configure_usage_dashboard_read_transaction(&mut tx).await?;

        let summary_row = account_usage_risk_summary_query(&query, cost_target_multiplier)
            .build()
            .fetch_one(&mut *tx)
            .await?;
        let totals = account_usage_risk_totals_from_row(&summary_row)?;
        let raw_cache = account_usage_risk_cache_stats_from_row(&summary_row, "raw")?;
        let reported_cache = account_usage_risk_cache_stats_from_row(&summary_row, "reported")?;
        let cost = account_usage_risk_cost_stats_from_row(&summary_row)?;

        let bucket_rows = account_usage_risk_bucket_query(&query, cost_target_multiplier)
            .build()
            .fetch_all(&mut *tx)
            .await?;
        let buckets = bucket_rows
            .into_iter()
            .map(account_usage_risk_bucket_from_row)
            .collect::<anyhow::Result<Vec<_>>>()?;

        let group_rows = account_usage_risk_group_query(&query, cost_target_multiplier)
            .build()
            .fetch_all(&mut *tx)
            .await?;
        let mut by_pool = Vec::new();
        let mut by_path = Vec::new();
        let mut by_model = Vec::new();
        for row in group_rows {
            let group_kind: String = row.try_get("group_kind")?;
            let group = account_usage_risk_group_from_row(&row)?;
            match group_kind.as_str() {
                "pool" => by_pool.push(group),
                "path" => by_path.push(group),
                "model" => by_model.push(group),
                _ => {}
            }
        }

        let sample_rows = account_usage_risk_sample_query(&query, cost_target_multiplier)
            .build()
            .fetch_all(&mut *tx)
            .await?;
        let samples = sample_rows
            .into_iter()
            .map(account_usage_risk_sample_from_row)
            .collect::<anyhow::Result<Vec<_>>>()?;

        tx.commit().await?;

        Ok(UsageAccountRiskResponse {
            generated_at: Utc::now().to_rfc3339(),
            timezone: query.timezone.clone(),
            window: UsageAccountRiskWindow {
                key: query.window_key.clone(),
                label: query.window_label.clone(),
                from: query.from.to_rfc3339(),
                to: query.to.to_rfc3339(),
            },
            thresholds: UsageAccountRiskThresholds {
                warning_tokens: query.warning_threshold_tokens,
                critical_tokens: query.critical_threshold_tokens,
                cost_floor_enabled: cost_config.cost_floor_enabled,
                cost_floor_margin_percent: cost_config.cost_floor_margin_percent,
                cost_target_multiplier,
            },
            filters: UsageAccountRiskFilters {
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
                COALESCE(SUM(s.priced_requests), 0)::bigint AS priced_requests,
                COALESCE(SUM(s.unpriced_requests), 0)::bigint AS unpriced_requests,
                CASE
                    WHEN COALESCE(SUM(s.duration_ms_count), 0) > 0
                    THEN COALESCE(SUM(s.duration_ms_sum), 0)::double precision
                         / SUM(s.duration_ms_count)::double precision
                    ELSE 0
                END AS average_duration_ms,


                COALESCE(SUM(s.simulated_requests), 0)::bigint AS simulated_requests,
                COALESCE(SUM(s.upstream_metadata_requests), 0)::bigint AS upstream_metadata_requests,
                COALESCE(SUM(s.account_requests), 0)::bigint AS account_requests,
                COALESCE(SUM(s.account_priced_requests), 0)::bigint AS account_priced_requests,
                COALESCE(SUM(s.account_unpriced_requests), 0)::bigint AS account_unpriced_requests,
                COALESCE(SUM(s.account_cost_floor_applied_requests), 0)::bigint AS account_cost_floor_applied_requests,
                COALESCE(SUM(s.account_raw_cost_usd), 0)::double precision AS account_raw_cost_usd,
                COALESCE(SUM(
                    CASE
                        WHEN COALESCE(s.account_shaped_cost_usd, 0) <> 0
                        THEN s.account_shaped_cost_usd
                        ELSE COALESCE(s.account_reported_cost_usd, 0)
                    END
                ), 0)::double precision AS account_shaped_cost_usd,
                COALESCE(SUM(
                    CASE
                        WHEN COALESCE(s.account_uplifted_cost_usd, 0) <> 0
                        THEN s.account_uplifted_cost_usd
                        ELSE COALESCE(s.account_reported_cost_usd, 0)
                    END
                ), 0)::double precision AS account_uplifted_cost_usd,
                COALESCE(SUM(
                    CASE
                        WHEN COALESCE(s.account_profit_usd, 0) <> 0
                        THEN s.account_profit_usd
                        WHEN COALESCE(s.account_reported_cost_usd, 0) <> 0
                          OR COALESCE(s.account_raw_cost_usd, 0) <> 0
                        THEN COALESCE(s.account_reported_cost_usd, 0) - COALESCE(s.account_raw_cost_usd, 0)
                        ELSE 0
                    END
                ), 0)::double precision AS account_profit_usd,
                COALESCE(SUM(s.account_reported_cost_usd), 0)::double precision AS account_reported_cost_usd,
                COALESCE(SUM(s.account_billable_cost_usd), 0)::double precision AS account_billable_cost_usd,
                COALESCE(SUM(s.account_cost_floor_delta_usd), 0)::double precision AS account_cost_floor_delta_usd
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

                r.priced_requests,
                r.unpriced_requests,
                r.average_duration_ms,
                COALESCE(p.p95_duration_ms, 0)::bigint AS p95_duration_ms,


                r.simulated_requests,
                r.upstream_metadata_requests,
                r.account_requests,
                r.account_priced_requests,
                r.account_unpriced_requests,
                r.account_cost_floor_applied_requests,
                r.account_raw_cost_usd,
                r.account_shaped_cost_usd,
                r.account_uplifted_cost_usd,
                r.account_profit_usd,
                r.account_reported_cost_usd,
                r.account_billable_cost_usd,
                r.account_cost_floor_delta_usd
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
        let mut account_billing = self.dashboard_account_billing_by_pool(specs).await?;

        for window in windows {
            window.summary.status_breakdown =
                status_breakdown.remove(&window.key).unwrap_or_default();
            window.summary.usage_source_breakdown = usage_source_breakdown
                .remove(&window.key)
                .unwrap_or_default();
            window.summary.account_billing_by_pool =
                account_billing.remove(&window.key).unwrap_or_default();
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
                ), 0)::double precision AS total_original_cost_usd
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

    async fn dashboard_account_billing_by_pool(
        &self,
        specs: &[UsageDashboardWindowSpec],
    ) -> anyhow::Result<HashMap<String, Vec<UsageAccountBillingByPool>>> {
        if specs.is_empty() {
            return Ok(HashMap::new());
        }

        let mut builder = QueryBuilder::<Postgres>::new("");
        push_dashboard_windows_cte(&mut builder, specs);
        push_dashboard_account_segments_cte(&mut builder);
        builder.push(
            r#"
            SELECT
                w.key AS window_key,
                s.pool_id,
                NULLIF(MAX(s.pool_name), '') AS pool_name,
                COALESCE(SUM(s.account_requests), 0)::bigint AS requests,
                COALESCE(SUM(s.account_priced_requests), 0)::bigint AS priced_requests,
                COALESCE(SUM(s.account_unpriced_requests), 0)::bigint AS unpriced_requests,
                COALESCE(SUM(s.account_cost_floor_applied_requests), 0)::bigint AS cost_floor_applied_requests,
                COALESCE(SUM(s.account_raw_cost_usd), 0)::double precision AS raw_cost_usd,
                COALESCE(SUM(
                    CASE
                        WHEN COALESCE(s.account_shaped_cost_usd, 0) <> 0
                        THEN s.account_shaped_cost_usd
                        ELSE COALESCE(s.account_reported_cost_usd, 0)
                    END
                ), 0)::double precision AS shaped_cost_usd,
                COALESCE(SUM(
                    CASE
                        WHEN COALESCE(s.account_uplifted_cost_usd, 0) <> 0
                        THEN s.account_uplifted_cost_usd
                        ELSE COALESCE(s.account_reported_cost_usd, 0)
                    END
                ), 0)::double precision AS uplifted_cost_usd,
                COALESCE(SUM(
                    CASE
                        WHEN COALESCE(s.account_profit_usd, 0) <> 0
                        THEN s.account_profit_usd
                        WHEN COALESCE(s.account_reported_cost_usd, 0) <> 0
                          OR COALESCE(s.account_raw_cost_usd, 0) <> 0
                        THEN COALESCE(s.account_reported_cost_usd, 0) - COALESCE(s.account_raw_cost_usd, 0)
                        ELSE 0
                    END
                ), 0)::double precision AS profit_usd,
                COALESCE(SUM(s.account_reported_cost_usd), 0)::double precision AS reported_cost_usd,
                COALESCE(SUM(s.account_billable_cost_usd), 0)::double precision AS billable_cost_usd,
                COALESCE(SUM(s.account_cost_floor_delta_usd), 0)::double precision AS cost_floor_delta_usd
            FROM window_bounds w
            JOIN dashboard_account_segments s ON s.window_key = w.key
            GROUP BY w.key, w.ord, s.pool_id
            HAVING COALESCE(SUM(s.account_requests), 0) > 0
            ORDER BY w.ord, uplifted_cost_usd DESC, requests DESC, pool_id
            "#,
        );

        let mut tx = self.store.pool().begin().await?;
        configure_usage_dashboard_read_transaction(&mut tx).await?;
        let rows = builder.build().fetch_all(&mut *tx).await?;
        tx.commit().await?;
        let mut grouped: HashMap<String, Vec<UsageAccountBillingByPool>> = HashMap::new();
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
                .push(UsageAccountBillingByPool {
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
                END::double precision AS total_original_cost_usd
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
                ), 0)::double precision AS total_original_cost_usd
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
                SUM(total_original_cost_usd)::double precision AS total_original_cost_usd
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

fn account_usage_risk_summary_query<'a>(
    query: &'a UsageAccountRiskQuery,
    cost_target_multiplier: f64,
) -> QueryBuilder<'a, Postgres> {
    let mut builder = QueryBuilder::<Postgres>::new("");
    push_account_usage_risk_scored_cte(&mut builder, query, cost_target_multiplier);
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
            COUNT(*) FILTER (WHERE NOT account_billing_present)::bigint
                AS missing_account_billing_records,
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

fn account_usage_risk_bucket_query<'a>(
    query: &'a UsageAccountRiskQuery,
    cost_target_multiplier: f64,
) -> QueryBuilder<'a, Postgres> {
    let mut builder = QueryBuilder::<Postgres>::new("");
    push_account_usage_risk_scored_cte(&mut builder, query, cost_target_multiplier);
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

fn account_usage_risk_group_query<'a>(
    query: &'a UsageAccountRiskQuery,
    cost_target_multiplier: f64,
) -> QueryBuilder<'a, Postgres> {
    let mut builder = QueryBuilder::<Postgres>::new("");
    push_account_usage_risk_scored_cte(&mut builder, query, cost_target_multiplier);
    builder.push(
        r#"
        , group_rows AS (
            SELECT
                'pool' AS group_kind,
                COALESCE(account_id_text, 'unknown') AS group_key,
                COALESCE(
                    account_name,
                    CASE
                        WHEN account_id_text IS NOT NULL THEN '#' || account_id_text
                        ELSE '未知账号'
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

fn account_usage_risk_sample_query<'a>(
    query: &'a UsageAccountRiskQuery,
    cost_target_multiplier: f64,
) -> QueryBuilder<'a, Postgres> {
    let mut builder = QueryBuilder::<Postgres>::new("");
    push_account_usage_risk_scored_cte(&mut builder, query, cost_target_multiplier);
    builder.push(
        r#"
        SELECT
            id,
            created_at,
            endpoint,
            stream,
            model,
            status,
            account_id_text,
            account_name,
            pricing_model,
            usage_projection_mode,
            account_billing_present,
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
                CASE WHEN NOT account_billing_present THEN 'missing_account_billing' END,
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

fn push_account_usage_risk_scored_cte<'a>(
    builder: &mut QueryBuilder<'a, Postgres>,
    query: &'a UsageAccountRiskQuery,
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
                NULLIF(records.data->>'accountId', '') AS account_id_text,
                NULLIF(BTRIM(records.data->>'accountName'), '') AS account_name,
                COALESCE(
                    NULLIF(BTRIM(records.data #>> '{accountBilling,pricingModel}'), ''),
                    NULLIF(BTRIM(records.pricing_model), '')
                ) AS pricing_model,
                NULLIF(BTRIM(records.data #>> '{accountBilling,usageProjectionMode}'), '')
                    AS usage_projection_mode,
                COALESCE(jsonb_typeof(records.data->'accountBilling') = 'object', false)
                    AS account_billing_present,
                COALESCE(
                    jsonb_typeof(records.data #> '{accountBilling,rawUsage}') = 'object',
                    false
                )
                    AS raw_usage_present,
                COALESCE(
                    jsonb_typeof(records.data #> '{accountBilling,reportedUsage}') = 'object',
                    false
                )
                    AS reported_usage_present,
                COALESCE(records.data #> '{accountBilling,costFloorApplied}' = 'true'::jsonb, false)
                    AS cost_floor_applied,
                CASE
                    WHEN jsonb_typeof(records.data #> '{accountBilling,rawUsage,inputTokens}') = 'number'
                    THEN GREATEST(
                        LEAST(
                            (records.data #>> '{accountBilling,rawUsage,inputTokens}')::numeric,
                            9223372036854775807::numeric
                        ),
                        0::numeric
                    )::bigint
                    ELSE 0::bigint
                END AS raw_input_tokens,
                CASE
                    WHEN jsonb_typeof(records.data #> '{accountBilling,rawUsage,outputTokens}') = 'number'
                    THEN GREATEST(
                        LEAST(
                            (records.data #>> '{accountBilling,rawUsage,outputTokens}')::numeric,
                            9223372036854775807::numeric
                        ),
                        0::numeric
                    )::bigint
                    ELSE 0::bigint
                END AS raw_output_tokens,
                CASE
                    WHEN jsonb_typeof(records.data #> '{accountBilling,rawUsage,cacheReadInputTokens}') = 'number'
                    THEN GREATEST(
                        LEAST(
                            (records.data #>> '{accountBilling,rawUsage,cacheReadInputTokens}')::numeric,
                            9223372036854775807::numeric
                        ),
                        0::numeric
                    )::bigint
                    ELSE 0::bigint
                END AS raw_cache_read_input_tokens,
                CASE
                    WHEN jsonb_typeof(records.data #> '{accountBilling,rawUsage,cacheCreationInputTokens}') = 'number'
                    THEN GREATEST(
                        LEAST(
                            (records.data #>> '{accountBilling,rawUsage,cacheCreationInputTokens}')::numeric,
                            9223372036854775807::numeric
                        ),
                        0::numeric
                    )::bigint
                    ELSE 0::bigint
                END AS raw_cache_creation_input_tokens,
                CASE
                    WHEN jsonb_typeof(records.data #> '{accountBilling,reportedUsage,inputTokens}') = 'number'
                    THEN GREATEST(
                        LEAST(
                            (records.data #>> '{accountBilling,reportedUsage,inputTokens}')::numeric,
                            9223372036854775807::numeric
                        ),
                        0::numeric
                    )::bigint
                    ELSE GREATEST(records.compat_input_tokens, 0)::bigint
                END AS reported_input_tokens,
                CASE
                    WHEN jsonb_typeof(records.data #> '{accountBilling,reportedUsage,outputTokens}') = 'number'
                    THEN GREATEST(
                        LEAST(
                            (records.data #>> '{accountBilling,reportedUsage,outputTokens}')::numeric,
                            9223372036854775807::numeric
                        ),
                        0::numeric
                    )::bigint
                    ELSE GREATEST(records.output_tokens, 0)::bigint
                END AS reported_output_tokens,
                CASE
                    WHEN jsonb_typeof(records.data #> '{accountBilling,reportedUsage,cacheReadInputTokens}') = 'number'
                    THEN GREATEST(
                        LEAST(
                            (records.data #>> '{accountBilling,reportedUsage,cacheReadInputTokens}')::numeric,
                            9223372036854775807::numeric
                        ),
                        0::numeric
                    )::bigint
                    ELSE GREATEST(records.cache_read_input_tokens, 0)::bigint
                END AS reported_cache_read_input_tokens,
                CASE
                    WHEN jsonb_typeof(records.data #> '{accountBilling,reportedUsage,cacheCreationInputTokens}') = 'number'
                    THEN GREATEST(
                        LEAST(
                            (records.data #>> '{accountBilling,reportedUsage,cacheCreationInputTokens}')::numeric,
                            9223372036854775807::numeric
                        ),
                        0::numeric
                    )::bigint
                    ELSE GREATEST(records.cache_creation_input_tokens, 0)::bigint
                END AS reported_cache_creation_input_tokens,
                CASE
                    WHEN jsonb_typeof(records.data #> '{accountBilling,rawCostUsd}') = 'number'
                    THEN (records.data #>> '{accountBilling,rawCostUsd}')::double precision
                    WHEN COALESCE(records.original_cost_usd, 0) <> 0
                    THEN records.original_cost_usd
                    ELSE 0::double precision
                END AS raw_cost_usd,
                CASE
                    WHEN jsonb_typeof(records.data #> '{accountBilling,reportedCostUsd}') = 'number'
                    THEN (records.data #>> '{accountBilling,reportedCostUsd}')::double precision
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
                  records.data->>'routeKind' = 'account'
                  OR jsonb_typeof(records.data->'accountBilling') = 'object'
              )
        "#,
    );

    if let Some(pool_id) = query.pool_id {
        builder.push(" AND records.data->>'accountId' = ");
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
        builder.push(" OR records.data #>> '{accountBilling,pricingModel}' = ");
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
                    OR NOT account_billing_present
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

fn account_usage_risk_totals_from_row(row: &PgRow) -> anyhow::Result<UsageAccountRiskTotals> {
    Ok(UsageAccountRiskTotals {
        records: row_i64_to_usize(row, "records")?,
        success_records: row_i64_to_usize(row, "success_records")?,
        error_records: row_i64_to_usize(row, "error_records")?,
        stream_records: row_i64_to_usize(row, "stream_records")?,
        non_stream_records: row_i64_to_usize(row, "non_stream_records")?,
        priced_records: row_i64_to_usize(row, "priced_records")?,
        unpriced_records: row_i64_to_usize(row, "unpriced_records")?,
        raw_usage_records: row_i64_to_usize(row, "raw_usage_records")?,
        reported_usage_records: row_i64_to_usize(row, "reported_usage_records")?,
        missing_account_billing_records: row_i64_to_usize(row, "missing_account_billing_records")?,
        output_zero_records: row_i64_to_usize(row, "output_zero_records")?,
    })
}

fn account_usage_risk_cache_stats_from_row(
    row: &PgRow,
    prefix: &str,
) -> anyhow::Result<UsageAccountRiskCacheStats> {
    Ok(UsageAccountRiskCacheStats {
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

fn account_usage_risk_cost_stats_from_row(
    row: &PgRow,
) -> anyhow::Result<UsageAccountRiskCostStats> {
    Ok(UsageAccountRiskCostStats {
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

fn account_usage_risk_bucket_from_row(row: PgRow) -> anyhow::Result<UsageAccountRiskBucket> {
    Ok(UsageAccountRiskBucket {
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

fn account_usage_risk_group_from_row(row: &PgRow) -> anyhow::Result<UsageAccountRiskGroup> {
    Ok(UsageAccountRiskGroup {
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

fn account_usage_risk_sample_from_row(row: PgRow) -> anyhow::Result<UsageAccountRiskSample> {
    let created_at: DateTime<Utc> = row.try_get("created_at")?;
    Ok(UsageAccountRiskSample {
        id: row.try_get("id")?,
        created_at: created_at.to_rfc3339(),
        endpoint: row.try_get("endpoint")?,
        stream: row.try_get("stream")?,
        model: row.try_get("model")?,
        status: row.try_get("status")?,
        account_id: row
            .try_get::<Option<String>, _>("account_id_text")?
            .and_then(|value| value.parse::<u64>().ok()),
        account_name: row.try_get("account_name")?,
        pricing_model: row.try_get("pricing_model")?,
        usage_projection_mode: row.try_get("usage_projection_mode")?,
        account_billing_present: row.try_get("account_billing_present")?,
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

    if let Some(account_billing) = record
        .account_billing
        .as_ref()
        .filter(|billing| billing.raw_cost_usd != 0.0)
    {
        record.original_cost_usd = account_billing.raw_cost_usd;
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
            id, created_at, endpoint, stream, model, conversation_id, account_id,
            account_label, status, usage_source, total_input_tokens, compat_input_tokens,
            billable_input_tokens, output_tokens, cache_read_input_tokens,
            cache_creation_input_tokens, cache_creation_5m_input_tokens,
            cache_creation_1h_input_tokens, estimated_cost_usd, original_cost_usd,
            pricing_available, pricing_model, duration_ms, simulated,
            error_type, error_message, error_detail, data, account_cost_usd
        )
        SELECT
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
            $11, $12, $13, $14, $15, $16, $17, $18,
            $19, $20, $21, $22, $23, $24, $25, $26, $27, $28, $29
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
            account_id = EXCLUDED.account_id,
            account_label = EXCLUDED.account_label,
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

            pricing_available = EXCLUDED.pricing_available,
            pricing_model = EXCLUDED.pricing_model,
            duration_ms = EXCLUDED.duration_ms,
            simulated = EXCLUDED.simulated,


            error_type = EXCLUDED.error_type,
            error_message = EXCLUDED.error_message,
            error_detail = EXCLUDED.error_detail,
            data = EXCLUDED.data,
            account_cost_usd = EXCLUDED.account_cost_usd,
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
    .bind(record.pricing_available)
    .bind(&record.pricing_model)
    .bind(u64_to_i64(record.duration_ms))
    .bind(record.simulated)
    .bind(&record.error_type)
    .bind(&record.error_message)
    .bind(&record.error_detail)
    .bind(value)
    .bind(record.account_cost_usd)
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

/// A transaction left open this long without a statement is ended by PostgreSQL itself.
/// Every transaction in this code base is a short burst of statements, so an idle open
/// transaction can only be one whose client future was cancelled mid-way.
const IDLE_IN_TRANSACTION_SESSION_TIMEOUT_MS: u64 = 60_000;

/// Pool options for the production pools.
///
/// sqlx 0.8 cannot always roll back a transaction whose future is cancelled while `BEGIN` is
/// in flight (for example by an outer `tokio::time::timeout`): the connection goes back to the
/// pool inside an open transaction, the next user's `BEGIN` warns "there is already a
/// transaction in progress", and its statements join the stale transaction. Two guards:
/// - the server ends such a transaction after `IDLE_IN_TRANSACTION_SESSION_TIMEOUT_MS`;
/// - a connection returned to the pool inside a transaction is closed instead of reused.
fn production_pool_options(max_connections: u32) -> PgPoolOptions {
    PgPoolOptions::new()
        .max_connections(max_connections.max(1))
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                sqlx::query(&format!(
                    "SET idle_in_transaction_session_timeout = {IDLE_IN_TRANSACTION_SESSION_TIMEOUT_MS}"
                ))
                .execute(conn)
                .await?;
                Ok(())
            })
        })
        .after_release(|conn, _meta| {
            Box::pin(async move {
                outside_transaction(conn).await
            })
        })
}

/// Whether the connection is outside any transaction. Inside one, `now()` stays at the
/// transaction start while `statement_timestamp()` moves with each statement. This must run
/// over the simple query protocol: with the extended protocol the Execute message moves
/// `statement_timestamp()` past the start of the implicit transaction even when no explicit
/// transaction is open.
async fn outside_transaction(conn: &mut PgConnection) -> Result<bool, sqlx::Error> {
    // A plain `&str` carries no arguments, so sqlx sends it as a simple Query message.
    let row = sqlx::Executor::fetch_one(conn, "SELECT now() = statement_timestamp()").await?;
    let outside_transaction: bool = row.try_get(0)?;
    if !outside_transaction {
        tracing::warn!("PgSQL 连接归还时仍处于未结束的事务中，关闭该连接而不是放回连接池");
    }
    Ok(outside_transaction)
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

/// Advisory locks are database-wide, but every test runs in its own schema. Scope the migration
/// lock by schema in tests so parallel tests do not queue behind each other's migrations;
/// production keeps the single global lock id.
#[cfg(test)]
async fn postgres_migration_lock_id(conn: &mut PgConnection) -> anyhow::Result<i64> {
    let schema: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(&mut *conn)
        .await?;
    let digest = Sha256::digest(schema.as_bytes());
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    Ok(POSTGRES_MIGRATION_LOCK_ID ^ i64::from_be_bytes(bytes))
}

#[cfg(not(test))]
async fn postgres_migration_lock_id(_conn: &mut PgConnection) -> anyhow::Result<i64> {
    Ok(POSTGRES_MIGRATION_LOCK_ID)
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
        include_summary: row.try_get("include_summary")?,
        cutoff_at: row.try_get("cutoff_at")?,
        batch_size: nonnegative_int("batch_size")?,
        max_batches: nonnegative_int("max_batches")?,
        max_rows: optional_u64("max_rows")?,
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

    total_input_tokens: i64,
    billable_input_tokens: i64,
    /// Uncached input as reported downstream (Anthropic `input_tokens`); the
    /// cache hit-rate denominator is this + cache read + cache creation.
    total_compat_input_tokens: i64,
    total_output_tokens: i64,
    total_cache_read_input_tokens: i64,
    total_cache_creation_input_tokens: i64,
    local_prompt_cache_input_tokens: i64,
    local_prompt_cache_read_input_tokens: i64,
    local_prompt_cache_creation_input_tokens: i64,
    total_estimated_cost_usd: f64,
    total_original_cost_usd: f64,

    account_requests: i64,
    account_priced_requests: i64,
    account_unpriced_requests: i64,
    account_cost_floor_applied_requests: i64,
    account_raw_cost_usd: f64,
    account_shaped_cost_usd: f64,
    account_uplifted_cost_usd: f64,
    account_profit_usd: f64,
    account_reported_cost_usd: f64,
    account_billable_cost_usd: f64,
    account_cost_floor_delta_usd: f64,
    duration_ms_sum: i64,
    duration_ms_count: i64,
    duration_ms_max: i64,
    /// 账号成本：上游原始成本 × 账号倍率（`UsageRecord::account_cost_usd`）。
    account_cost_usd: f64,
    /// 上游原始用量的输入 + 输出 token，总览用来和下游报告量做对照。
    account_raw_tokens: i64,
}

impl UsageRollupMetrics {
    fn from_record(record: &UsageRecord, direction: i64) -> Self {
        let sign = if direction < 0 { -1 } else { 1 };
        let success = record.status == UsageRecordStatus::Success;
        let local_prompt_cache = record.usage_source == UsageSource::LocalPromptCache;
        let upstream_metadata = record.usage_source == UsageSource::UpstreamMetadata;
        let account = record.route_kind == Some(UsageRouteKind::Account);
        let external_billing = record.account_billing.as_ref();
        let external_priced =
            account && external_billing.is_some_and(|billing| billing.pricing_available);
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

            total_input_tokens: signed_i32(record.total_input_tokens, sign),
            billable_input_tokens: signed_i32(record.billable_input_tokens, sign),
            total_compat_input_tokens: signed_i32(record.compat_input_tokens, sign),
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

            account_requests: signed_bool(account, sign),
            account_priced_requests: signed_bool(external_priced, sign),
            account_unpriced_requests: signed_bool(account && !external_priced, sign),
            account_cost_floor_applied_requests: signed_bool(
                external_billing.is_some_and(|billing| billing.cost_floor_applied),
                sign,
            ),
            account_raw_cost_usd: external_billing
                .map(|billing| billing.raw_cost_usd * sign as f64)
                .unwrap_or(0.0),
            account_shaped_cost_usd: external_billing
                .map(|billing| billing.effective_shaped_cost_usd() * sign as f64)
                .unwrap_or(0.0),
            account_uplifted_cost_usd: external_billing
                .map(|billing| billing.effective_uplifted_cost_usd() * sign as f64)
                .unwrap_or(0.0),
            account_profit_usd: external_billing
                .map(|billing| billing.effective_profit_usd() * sign as f64)
                .unwrap_or(0.0),
            account_reported_cost_usd: external_billing
                .map(|billing| billing.reported_cost_usd * sign as f64)
                .unwrap_or(0.0),
            account_billable_cost_usd: external_billing
                .map(|billing| billing.billable_cost_usd * sign as f64)
                .unwrap_or(0.0),
            account_cost_floor_delta_usd: external_billing
                .map(|billing| billing.cost_floor_delta_usd * sign as f64)
                .unwrap_or(0.0),
            duration_ms_sum: (record.duration_ms.min(i64::MAX as u64) as i64) * sign,
            duration_ms_count: sign,
            duration_ms_max: if sign > 0 {
                record.duration_ms.min(i64::MAX as u64) as i64
            } else {
                0
            },
            account_cost_usd: record.account_cost_usd * sign as f64,
            account_raw_tokens: usage_record_raw_tokens(record) * sign,
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
        self.total_input_tokens += other.total_input_tokens;
        self.billable_input_tokens += other.billable_input_tokens;
        self.total_compat_input_tokens += other.total_compat_input_tokens;
        self.total_output_tokens += other.total_output_tokens;
        self.total_cache_read_input_tokens += other.total_cache_read_input_tokens;
        self.total_cache_creation_input_tokens += other.total_cache_creation_input_tokens;
        self.local_prompt_cache_input_tokens += other.local_prompt_cache_input_tokens;
        self.local_prompt_cache_read_input_tokens += other.local_prompt_cache_read_input_tokens;
        self.local_prompt_cache_creation_input_tokens +=
            other.local_prompt_cache_creation_input_tokens;
        self.total_estimated_cost_usd += other.total_estimated_cost_usd;
        self.total_original_cost_usd += other.total_original_cost_usd;
        self.account_requests += other.account_requests;
        self.account_priced_requests += other.account_priced_requests;
        self.account_unpriced_requests += other.account_unpriced_requests;
        self.account_cost_floor_applied_requests += other.account_cost_floor_applied_requests;
        self.account_raw_cost_usd += other.account_raw_cost_usd;
        self.account_shaped_cost_usd += other.account_shaped_cost_usd;
        self.account_uplifted_cost_usd += other.account_uplifted_cost_usd;
        self.account_profit_usd += other.account_profit_usd;
        self.account_reported_cost_usd += other.account_reported_cost_usd;
        self.account_billable_cost_usd += other.account_billable_cost_usd;
        self.account_cost_floor_delta_usd += other.account_cost_floor_delta_usd;
        self.duration_ms_sum += other.duration_ms_sum;
        self.duration_ms_count += other.duration_ms_count;
        self.duration_ms_max = self.duration_ms_max.max(other.duration_ms_max);
        self.account_cost_usd += other.account_cost_usd;
        self.account_raw_tokens += other.account_raw_tokens;
    }
}

/// 上游原始用量的输入 + 输出 token。优先取账号计费里的原始快照，老记录退回顶层 `raw_usage`。
fn usage_record_raw_tokens(record: &UsageRecord) -> i64 {
    let raw = record
        .account_billing
        .as_ref()
        .map(|billing| billing.raw_usage)
        .or(record.raw_usage);
    raw.map(|usage| i64::from(usage.total_input_tokens) + i64::from(usage.output_tokens))
        .unwrap_or(0)
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
struct UsageRollupBatchDelta {
    totals: HashMap<(&'static str, String), UsageRollupAggregate>,
    time_buckets: HashMap<(DateTime<Utc>, &'static str, String), UsageRollupAggregate>,
    cache_read_totals: HashMap<i32, i64>,
    cache_read_time_buckets: HashMap<(DateTime<Utc>, i32), i64>,
    duration_time_buckets: HashMap<(DateTime<Utc>, i32), i64>,
    has_negative: bool,
}

/// Accumulates the overview totals (`account_cost_usd`, raw tokens) per rollup row for the
/// targeted backfill of the two columns added after launch.
#[derive(Default)]
struct UsageOverviewTotalsBackfill {
    totals: HashMap<(&'static str, String), (f64, i64)>,
    time_buckets: HashMap<(DateTime<Utc>, &'static str, String), (f64, i64)>,
}

impl UsageOverviewTotalsBackfill {
    fn add_record(&mut self, record: &UsageRecord) {
        let cost = record.account_cost_usd;
        let raw_tokens = usage_record_raw_tokens(record);
        if cost == 0.0 && raw_tokens == 0 {
            return;
        }
        let bucket_start = usage_rollup_bucket_start(parse_usage_created_at(record));
        for dimension in usage_rollup_dimensions(record) {
            let total = self
                .totals
                .entry((dimension.dimension, dimension.key.clone()))
                .or_default();
            total.0 += cost;
            total.1 += raw_tokens;
            if dimension.include_time_bucket {
                let bucket = self
                    .time_buckets
                    .entry((bucket_start, dimension.dimension, dimension.key))
                    .or_default();
                bucket.0 += cost;
                bucket.1 += raw_tokens;
            }
        }
    }

    async fn apply(self, tx: &mut Transaction<'_, Postgres>) -> anyhow::Result<()> {
        const CHUNK: usize = 5_000;
        let totals = self.totals.into_iter().collect::<Vec<_>>();
        for chunk in totals.chunks(CHUNK) {
            let dimensions = chunk
                .iter()
                .map(|((d, _), _)| d.to_string())
                .collect::<Vec<_>>();
            let keys = chunk
                .iter()
                .map(|((_, k), _)| k.clone())
                .collect::<Vec<_>>();
            let costs = chunk.iter().map(|(_, v)| v.0).collect::<Vec<_>>();
            let tokens = chunk.iter().map(|(_, v)| v.1).collect::<Vec<_>>();
            sqlx::query(
                r#"
                UPDATE usage_rollup_totals AS rollup
                SET account_cost_usd = source.cost,
                    account_raw_tokens = source.tokens
                FROM unnest($1::text[], $2::text[], $3::double precision[], $4::bigint[])
                    AS source(dimension, dimension_key, cost, tokens)
                WHERE rollup.dimension = source.dimension
                  AND rollup.dimension_key = source.dimension_key
                "#,
            )
            .bind(&dimensions)
            .bind(&keys)
            .bind(&costs)
            .bind(&tokens)
            .execute(&mut **tx)
            .await?;
        }
        let buckets = self.time_buckets.into_iter().collect::<Vec<_>>();
        for chunk in buckets.chunks(CHUNK) {
            let starts = chunk.iter().map(|((b, _, _), _)| *b).collect::<Vec<_>>();
            let dimensions = chunk
                .iter()
                .map(|((_, d, _), _)| d.to_string())
                .collect::<Vec<_>>();
            let keys = chunk
                .iter()
                .map(|((_, _, k), _)| k.clone())
                .collect::<Vec<_>>();
            let costs = chunk.iter().map(|(_, v)| v.0).collect::<Vec<_>>();
            let tokens = chunk.iter().map(|(_, v)| v.1).collect::<Vec<_>>();
            sqlx::query(
                r#"
                UPDATE usage_rollup_time_buckets AS rollup
                SET account_cost_usd = source.cost,
                    account_raw_tokens = source.tokens
                FROM unnest($1::timestamptz[], $2::text[], $3::text[], $4::double precision[], $5::bigint[])
                    AS source(bucket_start, dimension, dimension_key, cost, tokens)
                WHERE rollup.bucket_start = source.bucket_start
                  AND rollup.dimension = source.dimension
                  AND rollup.dimension_key = source.dimension_key
                "#,
            )
            .bind(&starts)
            .bind(&dimensions)
            .bind(&keys)
            .bind(&costs)
            .bind(&tokens)
            .execute(&mut **tx)
            .await?;
        }
        Ok(())
    }
}

/// Accumulates `compat_input_tokens` per rollup row for the targeted backfill.
#[derive(Default)]
struct UsageCompatInputBackfill {
    totals: HashMap<(&'static str, String), i64>,
    time_buckets: HashMap<(DateTime<Utc>, &'static str, String), i64>,
}

impl UsageCompatInputBackfill {
    fn add_record(&mut self, record: &UsageRecord) {
        let value = i64::from(record.compat_input_tokens);
        if value == 0 {
            return;
        }
        let bucket_start = usage_rollup_bucket_start(parse_usage_created_at(record));
        for dimension in usage_rollup_dimensions(record) {
            *self
                .totals
                .entry((dimension.dimension, dimension.key.clone()))
                .or_default() += value;
            if dimension.include_time_bucket {
                *self
                    .time_buckets
                    .entry((bucket_start, dimension.dimension, dimension.key))
                    .or_default() += value;
            }
        }
    }

    async fn apply(self, tx: &mut Transaction<'_, Postgres>) -> anyhow::Result<()> {
        const CHUNK: usize = 5_000;
        let totals = self.totals.into_iter().collect::<Vec<_>>();
        for chunk in totals.chunks(CHUNK) {
            let dimensions = chunk
                .iter()
                .map(|((d, _), _)| d.to_string())
                .collect::<Vec<_>>();
            let keys = chunk
                .iter()
                .map(|((_, k), _)| k.clone())
                .collect::<Vec<_>>();
            let values = chunk.iter().map(|(_, v)| *v).collect::<Vec<_>>();
            sqlx::query(
                r#"
                UPDATE usage_rollup_totals AS rollup
                SET total_compat_input_tokens = source.value
                FROM unnest($1::text[], $2::text[], $3::bigint[])
                    AS source(dimension, dimension_key, value)
                WHERE rollup.dimension = source.dimension
                  AND rollup.dimension_key = source.dimension_key
                "#,
            )
            .bind(&dimensions)
            .bind(&keys)
            .bind(&values)
            .execute(&mut **tx)
            .await?;
        }
        let buckets = self.time_buckets.into_iter().collect::<Vec<_>>();
        for chunk in buckets.chunks(CHUNK) {
            let starts = chunk.iter().map(|((b, _, _), _)| *b).collect::<Vec<_>>();
            let dimensions = chunk
                .iter()
                .map(|((_, d, _), _)| d.to_string())
                .collect::<Vec<_>>();
            let keys = chunk
                .iter()
                .map(|((_, _, k), _)| k.clone())
                .collect::<Vec<_>>();
            let values = chunk.iter().map(|(_, v)| *v).collect::<Vec<_>>();
            sqlx::query(
                r#"
                UPDATE usage_rollup_time_buckets AS rollup
                SET total_compat_input_tokens = source.value
                FROM unnest($1::timestamptz[], $2::text[], $3::text[], $4::bigint[])
                    AS source(bucket_start, dimension, dimension_key, value)
                WHERE rollup.bucket_start = source.bucket_start
                  AND rollup.dimension = source.dimension
                  AND rollup.dimension_key = source.dimension_key
                "#,
            )
            .bind(&starts)
            .bind(&dimensions)
            .bind(&keys)
            .bind(&values)
            .execute(&mut **tx)
            .await?;
        }
        Ok(())
    }
}

/// 上线后新增、历史记录需要显式重建才有数据的汇总维度。
/// `rebuild_usage_route_dimensions` 维护命令会删掉这些维度的旧行再从明细重算。
const USAGE_LATE_ADDED_ROLLUP_DIMENSIONS: [&str; 2] = [
    USAGE_ROLLUP_ACCOUNT_ATTEMPT_DIMENSION,
    USAGE_ROLLUP_REQUEST_KEY_REJECTION_DIMENSION,
];

impl UsageRollupBatchDelta {
    /// Adds only the named dimensions (totals and hour buckets). Used by the
    /// targeted backfill so the other rollup tables are left untouched.
    fn add_record_dimensions(&mut self, record: &UsageRecord, only: &[&str]) {
        let metrics = UsageRollupMetrics::from_record(record, 1);
        let bucket_start = usage_rollup_bucket_start(parse_usage_created_at(record));
        let regular = usage_rollup_dimensions(record)
            .into_iter()
            .map(|dimension| (dimension, metrics));
        let attempts = usage_rollup_attempt_entries(record, 1).into_iter();
        for (dimension, metrics) in regular
            .chain(attempts)
            .filter(|(dimension, _)| only.contains(&dimension.dimension))
        {
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
    }

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
        // 尝试维度每个尝试一份自己的指标，与整条记录的指标无关；清理抵消走同一条路。
        for (dimension, attempt_metrics) in usage_rollup_attempt_entries(record, direction) {
            self.totals
                .entry((dimension.dimension, dimension.key.clone()))
                .or_default()
                .add(dimension.label.clone(), attempt_metrics);
            if dimension.include_time_bucket {
                self.time_buckets
                    .entry((bucket_start, dimension.dimension, dimension.key))
                    .or_default()
                    .add(dimension.label, attempt_metrics);
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
    }

    async fn apply(self, tx: &mut Transaction<'_, Postgres>) -> anyhow::Result<()> {
        let UsageRollupBatchDelta {
            totals,
            time_buckets,
            cache_read_totals,
            cache_read_time_buckets,
            duration_time_buckets,
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
        if has_negative {
            reconcile_usage_rollups_after_negative_delta(
                tx,
                &affected_total_dimensions,
                &affected_total_keys,
                &affected_time_buckets,
                &affected_time_dimensions,
                &affected_time_keys,
                &affected_cache_reads,
                &affected_cache_buckets,
                &affected_duration_buckets,
            )
            .await?;
        }
        Ok(())
    }
}

/// 汇总维度名：按账号的每次尝试（含中途失败被切走的）。
const USAGE_ROLLUP_ACCOUNT_ATTEMPT_DIMENSION: &str = "account_attempt";
/// 汇总维度名：请求密钥的准入拒绝（限额、并发等 `request_rejection`）。
const USAGE_ROLLUP_REQUEST_KEY_REJECTION_DIMENSION: &str = "request_api_key_rejection";
/// 准入拒绝记录的 `error_type`，与 `usage.rs` 的 `REQUEST_REJECTION_ERROR_TYPE` 一致。
const USAGE_REQUEST_REJECTION_ERROR_TYPE: &str = "request_rejection";

/// 一条记录里每次账号尝试各自的汇总增量：每个尝试贡献一次 `requests`，
/// `action == "success"` 计成功、否则计错误；耗时取尝试自己的耗时，token/成本列为 0。
/// 最终成功的那次尝试也在内，所以 `account_attempt.requests >= account.requests`。
fn usage_rollup_attempt_entries(
    record: &UsageRecord,
    direction: i64,
) -> Vec<(UsageRollupDimension, UsageRollupMetrics)> {
    let sign = if direction < 0 { -1 } else { 1 };
    let last_index = record.external_attempts.len().saturating_sub(1);
    let earlier_duration_ms: u64 = record
        .external_attempts
        .iter()
        .take(last_index)
        .map(|attempt| attempt.duration_ms)
        .sum();
    record
        .external_attempts
        .iter()
        .enumerate()
        .map(|(index, attempt)| {
            // 拿到响应头时最后一次尝试就记成了 success；之后流中途出错或客户端断开，
            // 记录状态不再是 Success，这次尝试也要按失败计，和账号维度同一口径。
            // 它的耗时取整个请求耗时减去前面几次尝试，而不是只到响应头。
            let dispatched = index == last_index && attempt.action == "success";
            let success = if dispatched {
                record.status == UsageRecordStatus::Success
            } else {
                attempt.action == "success"
            };
            let duration_ms = if dispatched {
                record
                    .duration_ms
                    .saturating_sub(earlier_duration_ms)
                    .max(attempt.duration_ms)
            } else {
                attempt.duration_ms
            };
            let duration = duration_ms.min(i64::MAX as u64) as i64;
            let metrics = UsageRollupMetrics {
                requests: sign,
                success_requests: signed_bool(success, sign),
                error_requests: signed_bool(!success, sign),
                duration_ms_sum: duration * sign,
                duration_ms_count: sign,
                duration_ms_max: if sign > 0 { duration } else { 0 },
                ..UsageRollupMetrics::default()
            };
            let dimension = UsageRollupDimension {
                dimension: USAGE_ROLLUP_ACCOUNT_ATTEMPT_DIMENSION,
                key: attempt.pool_id.to_string(),
                label: Some(attempt.pool_name.trim())
                    .filter(|value| !value.is_empty())
                    .map(str::to_string),
                include_time_bucket: true,
            };
            (dimension, metrics)
        })
        .collect()
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

    let request_api_key = non_empty_or_unknown(record.request_api_key_id.as_deref().unwrap_or(""));
    dimensions.push(UsageRollupDimension {
        dimension: "request_api_key",
        key: request_api_key,
        label: None,
        include_time_bucket: true,
    });

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

    if let Some(account_id) = record.account_id {
        dimensions.push(UsageRollupDimension {
            dimension: "account",
            key: account_id.to_string(),
            label: record
                .account_name
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
        // 准入拒绝（限额、并发等）单独按请求密钥计数，安全页用它显示“24h 拒绝 N 次”。
        if record.error_type.as_deref() == Some(USAGE_REQUEST_REJECTION_ERROR_TYPE)
            && let Some(key_id) = record
                .request_api_key_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
        {
            dimensions.push(UsageRollupDimension {
                dimension: USAGE_ROLLUP_REQUEST_KEY_REJECTION_DIMENSION,
                key: key_id.to_string(),
                label: None,
                include_time_bucket: true,
            });
        }
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
            upstream_metadata_requests,
            total_input_tokens, billable_input_tokens, total_output_tokens,
            total_cache_read_input_tokens, total_cache_creation_input_tokens,
            local_prompt_cache_input_tokens, local_prompt_cache_read_input_tokens,
            local_prompt_cache_creation_input_tokens, total_estimated_cost_usd,
            total_original_cost_usd,
            account_requests, account_priced_requests, account_unpriced_requests,
            account_cost_floor_applied_requests, account_raw_cost_usd,
            account_shaped_cost_usd, account_uplifted_cost_usd,
            account_profit_usd, account_reported_cost_usd,
            account_billable_cost_usd, account_cost_floor_delta_usd,
            duration_ms_sum, duration_ms_count, duration_ms_max, total_compat_input_tokens,
            account_cost_usd, account_raw_tokens, updated_at
        )
        VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
            $11, $12, $13, $14, $15, $16,
            $17, $18, $19, $20, $21, $22, $23,
            $24, $25, $26, $27, $28, $29, $30, $31,
            $32, $33, $34, $35, $36, $37, $38, $39, $40, now()
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


            total_input_tokens = usage_rollup_totals.total_input_tokens + EXCLUDED.total_input_tokens,
            billable_input_tokens = usage_rollup_totals.billable_input_tokens + EXCLUDED.billable_input_tokens,
            total_compat_input_tokens = usage_rollup_totals.total_compat_input_tokens + EXCLUDED.total_compat_input_tokens,
            total_output_tokens = usage_rollup_totals.total_output_tokens + EXCLUDED.total_output_tokens,
            total_cache_read_input_tokens = usage_rollup_totals.total_cache_read_input_tokens + EXCLUDED.total_cache_read_input_tokens,
            total_cache_creation_input_tokens = usage_rollup_totals.total_cache_creation_input_tokens + EXCLUDED.total_cache_creation_input_tokens,
            local_prompt_cache_input_tokens = usage_rollup_totals.local_prompt_cache_input_tokens + EXCLUDED.local_prompt_cache_input_tokens,
            local_prompt_cache_read_input_tokens = usage_rollup_totals.local_prompt_cache_read_input_tokens + EXCLUDED.local_prompt_cache_read_input_tokens,
            local_prompt_cache_creation_input_tokens = usage_rollup_totals.local_prompt_cache_creation_input_tokens + EXCLUDED.local_prompt_cache_creation_input_tokens,
            total_estimated_cost_usd = usage_rollup_totals.total_estimated_cost_usd + EXCLUDED.total_estimated_cost_usd,
            total_original_cost_usd = usage_rollup_totals.total_original_cost_usd + EXCLUDED.total_original_cost_usd,

            account_requests = usage_rollup_totals.account_requests + EXCLUDED.account_requests,
            account_priced_requests = usage_rollup_totals.account_priced_requests + EXCLUDED.account_priced_requests,
            account_unpriced_requests = usage_rollup_totals.account_unpriced_requests + EXCLUDED.account_unpriced_requests,
            account_cost_floor_applied_requests = usage_rollup_totals.account_cost_floor_applied_requests + EXCLUDED.account_cost_floor_applied_requests,
            account_raw_cost_usd = usage_rollup_totals.account_raw_cost_usd + EXCLUDED.account_raw_cost_usd,
            account_shaped_cost_usd = usage_rollup_totals.account_shaped_cost_usd + EXCLUDED.account_shaped_cost_usd,
            account_uplifted_cost_usd = usage_rollup_totals.account_uplifted_cost_usd + EXCLUDED.account_uplifted_cost_usd,
            account_profit_usd = usage_rollup_totals.account_profit_usd + EXCLUDED.account_profit_usd,
            account_reported_cost_usd = usage_rollup_totals.account_reported_cost_usd + EXCLUDED.account_reported_cost_usd,
            account_billable_cost_usd = usage_rollup_totals.account_billable_cost_usd + EXCLUDED.account_billable_cost_usd,
            account_cost_floor_delta_usd = usage_rollup_totals.account_cost_floor_delta_usd + EXCLUDED.account_cost_floor_delta_usd,
            duration_ms_sum = usage_rollup_totals.duration_ms_sum + EXCLUDED.duration_ms_sum,
            duration_ms_count = usage_rollup_totals.duration_ms_count + EXCLUDED.duration_ms_count,
            duration_ms_max = CASE
                WHEN usage_rollup_totals.duration_ms_count + EXCLUDED.duration_ms_count <= 0 THEN 0
                ELSE GREATEST(usage_rollup_totals.duration_ms_max, EXCLUDED.duration_ms_max)
            END,
            account_cost_usd = usage_rollup_totals.account_cost_usd + EXCLUDED.account_cost_usd,
            account_raw_tokens = usage_rollup_totals.account_raw_tokens + EXCLUDED.account_raw_tokens,
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

    .bind(metrics.account_requests)
    .bind(metrics.account_priced_requests)
    .bind(metrics.account_unpriced_requests)
    .bind(metrics.account_cost_floor_applied_requests)
    .bind(metrics.account_raw_cost_usd)
    .bind(metrics.account_shaped_cost_usd)
    .bind(metrics.account_uplifted_cost_usd)
    .bind(metrics.account_profit_usd)
    .bind(metrics.account_reported_cost_usd)
    .bind(metrics.account_billable_cost_usd)
    .bind(metrics.account_cost_floor_delta_usd)
    .bind(metrics.duration_ms_sum)
    .bind(metrics.duration_ms_count)
    .bind(metrics.duration_ms_max)
    .bind(metrics.total_compat_input_tokens)
    .bind(metrics.account_cost_usd)
    .bind(metrics.account_raw_tokens)
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
            simulated_requests, upstream_metadata_requests, total_input_tokens, billable_input_tokens,
            total_output_tokens, total_cache_read_input_tokens,
            total_cache_creation_input_tokens, local_prompt_cache_input_tokens,
            local_prompt_cache_read_input_tokens, local_prompt_cache_creation_input_tokens,
            total_estimated_cost_usd, total_original_cost_usd,
            account_requests, account_priced_requests,
            account_unpriced_requests, account_cost_floor_applied_requests,
            account_raw_cost_usd, account_shaped_cost_usd,
            account_uplifted_cost_usd, account_profit_usd, account_reported_cost_usd,
            account_billable_cost_usd, account_cost_floor_delta_usd,
            duration_ms_sum, duration_ms_count,
            duration_ms_max, total_compat_input_tokens, account_cost_usd, account_raw_tokens,
            updated_at
        )
        VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
            $11, $12, $13, $14, $15, $16,
            $17, $18, $19, $20, $21, $22, $23, $24, $25, $26, $27, $28, $29, $30, $31,
            $32, $33, $34, $35, $36, $37, $38, $39, $40, $41, now()
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


            total_input_tokens = usage_rollup_time_buckets.total_input_tokens + EXCLUDED.total_input_tokens,
            billable_input_tokens = usage_rollup_time_buckets.billable_input_tokens + EXCLUDED.billable_input_tokens,
            total_compat_input_tokens = usage_rollup_time_buckets.total_compat_input_tokens + EXCLUDED.total_compat_input_tokens,
            total_output_tokens = usage_rollup_time_buckets.total_output_tokens + EXCLUDED.total_output_tokens,
            total_cache_read_input_tokens = usage_rollup_time_buckets.total_cache_read_input_tokens + EXCLUDED.total_cache_read_input_tokens,
            total_cache_creation_input_tokens = usage_rollup_time_buckets.total_cache_creation_input_tokens + EXCLUDED.total_cache_creation_input_tokens,
            local_prompt_cache_input_tokens = usage_rollup_time_buckets.local_prompt_cache_input_tokens + EXCLUDED.local_prompt_cache_input_tokens,
            local_prompt_cache_read_input_tokens = usage_rollup_time_buckets.local_prompt_cache_read_input_tokens + EXCLUDED.local_prompt_cache_read_input_tokens,
            local_prompt_cache_creation_input_tokens = usage_rollup_time_buckets.local_prompt_cache_creation_input_tokens + EXCLUDED.local_prompt_cache_creation_input_tokens,
            total_estimated_cost_usd = usage_rollup_time_buckets.total_estimated_cost_usd + EXCLUDED.total_estimated_cost_usd,
            total_original_cost_usd = usage_rollup_time_buckets.total_original_cost_usd + EXCLUDED.total_original_cost_usd,

            account_requests = usage_rollup_time_buckets.account_requests + EXCLUDED.account_requests,
            account_priced_requests = usage_rollup_time_buckets.account_priced_requests + EXCLUDED.account_priced_requests,
            account_unpriced_requests = usage_rollup_time_buckets.account_unpriced_requests + EXCLUDED.account_unpriced_requests,
            account_cost_floor_applied_requests = usage_rollup_time_buckets.account_cost_floor_applied_requests + EXCLUDED.account_cost_floor_applied_requests,
            account_raw_cost_usd = usage_rollup_time_buckets.account_raw_cost_usd + EXCLUDED.account_raw_cost_usd,
            account_shaped_cost_usd = usage_rollup_time_buckets.account_shaped_cost_usd + EXCLUDED.account_shaped_cost_usd,
            account_uplifted_cost_usd = usage_rollup_time_buckets.account_uplifted_cost_usd + EXCLUDED.account_uplifted_cost_usd,
            account_profit_usd = usage_rollup_time_buckets.account_profit_usd + EXCLUDED.account_profit_usd,
            account_reported_cost_usd = usage_rollup_time_buckets.account_reported_cost_usd + EXCLUDED.account_reported_cost_usd,
            account_billable_cost_usd = usage_rollup_time_buckets.account_billable_cost_usd + EXCLUDED.account_billable_cost_usd,
            account_cost_floor_delta_usd = usage_rollup_time_buckets.account_cost_floor_delta_usd + EXCLUDED.account_cost_floor_delta_usd,
            duration_ms_sum = usage_rollup_time_buckets.duration_ms_sum + EXCLUDED.duration_ms_sum,
            duration_ms_count = usage_rollup_time_buckets.duration_ms_count + EXCLUDED.duration_ms_count,
            duration_ms_max = CASE
                WHEN usage_rollup_time_buckets.duration_ms_count + EXCLUDED.duration_ms_count <= 0 THEN 0
                ELSE GREATEST(usage_rollup_time_buckets.duration_ms_max, EXCLUDED.duration_ms_max)
            END,
            account_cost_usd = usage_rollup_time_buckets.account_cost_usd + EXCLUDED.account_cost_usd,
            account_raw_tokens = usage_rollup_time_buckets.account_raw_tokens + EXCLUDED.account_raw_tokens,
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

    .bind(metrics.account_requests)
    .bind(metrics.account_priced_requests)
    .bind(metrics.account_unpriced_requests)
    .bind(metrics.account_cost_floor_applied_requests)
    .bind(metrics.account_raw_cost_usd)
    .bind(metrics.account_shaped_cost_usd)
    .bind(metrics.account_uplifted_cost_usd)
    .bind(metrics.account_profit_usd)
    .bind(metrics.account_reported_cost_usd)
    .bind(metrics.account_billable_cost_usd)
    .bind(metrics.account_cost_floor_delta_usd)
    .bind(metrics.duration_ms_sum)
    .bind(metrics.duration_ms_count)
    .bind(metrics.duration_ms_max)
    .bind(metrics.total_compat_input_tokens)
    .bind(metrics.account_cost_usd)
    .bind(metrics.account_raw_tokens)
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
            Self::Credential => "r.account_id::text",
            Self::Endpoint => "COALESCE(NULLIF(BTRIM(r.endpoint), ''), 'unknown')",
            Self::Error => "COALESCE(NULLIF(BTRIM(r.error_type), ''), r.status)",
        }
    }

    fn detail_label_column(self) -> &'static str {
        match self {
            Self::Credential => "NULLIF(BTRIM(r.account_label), '')",
            Self::Error => "NULLIF(BTRIM(r.error_message), '')",
            _ => "NULL::text",
        }
    }

    fn detail_filter(self) -> Option<&'static str> {
        match self {
            Self::Credential => Some("r.account_id IS NOT NULL"),
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
            "account_label",
            "status",
            "usage_source",
            "error_type",
            "error_message",
            "error_detail",
            "pricing_model",
            "account_id::text",
            "data->>'upstreamModel'",
            "data->>'externalOutboundModel'",
            "data->>'accountId'",
            "data->>'accountName'",
            "data->>'routeKind'",
            "data->>'routeSubtype'",
            "data->>'modelResolutionSource'",
            "estimated_cost_usd::text",
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
    if let Some(route) = query
        .route
        .as_deref()
        .and_then(normalize_usage_route_filter)
    {
        // 入口固定是 `{前缀}/v1/...`；默认路由的前缀为空，即 `/v1/` 开头。
        let prefix = if route == USAGE_DEFAULT_ROUTE_KEY {
            ""
        } else {
            route.as_str()
        };
        builder.push(" AND endpoint LIKE ");
        builder.push_bind(format!("{}/v1/%", escape_like_pattern(prefix)));
        builder.push(" ESCAPE '\\'");
        // LIKE 只是粗筛（能走前缀匹配）；再按"最后一个 /v1/ 之前"精确比较，
        // 与总览分组、延迟分位、内存筛选同一口径（如 /dfcache/v1/v1/messages 归 /dfcache/v1）。
        builder.push(
            " AND COALESCE(NULLIF(rtrim(regexp_replace(endpoint, '^(.*)/v1(/.*)?$', '\\1'), '/'), ''), '/') = ",
        );
        builder.push_bind(route.clone());
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
        builder.push(" AND account_id = ");
        builder.push_bind(credential_id as i64);
    }
    if let Some(account_id) = query.account_id {
        builder.push(" AND data->>'accountId' = ");
        builder.push_bind(account_id.to_string());
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
            records.account_id,
            records.account_label,
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

            records.pricing_available,
            records.simulated,


            records.data->>'accountId' AS dashboard_account_id,
            NULLIF(BTRIM(records.data->>'accountName'), '')
                AS dashboard_account_name,
            CASE
                WHEN jsonb_typeof(records.data->'durationMs') = 'number'
                 AND records.data->>'durationMs' ~ '^[0-9]+$'
                THEN LEAST(
                    (records.data->>'durationMs')::numeric,
                    9223372036854775807::numeric
                )::bigint
                ELSE GREATEST(records.duration_ms, 0)
            END AS dashboard_duration_ms,
            records.data->>'routeKind' = 'account' AS dashboard_account,
            jsonb_typeof(records.data->'accountBilling') = 'object'
                AS dashboard_external_billing_present,
            COALESCE(
                records.data #> '{accountBilling,pricingAvailable}' = 'true'::jsonb,
                false
            ) AS dashboard_external_pricing_available,
            COALESCE(
                records.data #> '{accountBilling,costFloorApplied}' = 'true'::jsonb,
                false
            ) AS dashboard_external_cost_floor_applied,
            CASE
                WHEN jsonb_typeof(records.data #> '{accountBilling,rawCostUsd}') = 'number'
                THEN (records.data #>> '{accountBilling,rawCostUsd}')::double precision
                ELSE 0::double precision
            END AS dashboard_external_raw_cost_usd,
            CASE
                WHEN jsonb_typeof(records.data #> '{accountBilling,shapedCostUsd}') = 'number'
                THEN (records.data #>> '{accountBilling,shapedCostUsd}')::double precision
                ELSE 0::double precision
            END AS dashboard_external_shaped_cost_usd,
            CASE
                WHEN jsonb_typeof(records.data #> '{accountBilling,upliftedCostUsd}') = 'number'
                THEN (records.data #>> '{accountBilling,upliftedCostUsd}')::double precision
                ELSE 0::double precision
            END AS dashboard_external_uplifted_cost_usd,
            CASE
                WHEN jsonb_typeof(records.data #> '{accountBilling,reportedCostUsd}') = 'number'
                THEN (records.data #>> '{accountBilling,reportedCostUsd}')::double precision
                ELSE 0::double precision
            END AS dashboard_external_reported_cost_usd,
            CASE
                WHEN jsonb_typeof(records.data #> '{accountBilling,billableCostUsd}') = 'number'
                THEN (records.data #>> '{accountBilling,billableCostUsd}')::double precision
                ELSE 0::double precision
            END AS dashboard_external_billable_cost_usd,
            CASE
                WHEN jsonb_typeof(records.data #> '{accountBilling,costFloorDeltaUsd}') = 'number'
                THEN (records.data #>> '{accountBilling,costFloorDeltaUsd}')::double precision
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

            buckets.priced_requests,
            buckets.unpriced_requests,


            buckets.simulated_requests,
            buckets.upstream_metadata_requests,
            buckets.account_requests,
            buckets.account_priced_requests,
            buckets.account_unpriced_requests,
            buckets.account_cost_floor_applied_requests,
            buckets.account_raw_cost_usd,
            buckets.account_shaped_cost_usd,
            buckets.account_uplifted_cost_usd,
            buckets.account_profit_usd,
            buckets.account_reported_cost_usd,
            buckets.account_billable_cost_usd,
            buckets.account_cost_floor_delta_usd,
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
            COUNT(*) FILTER (WHERE records.pricing_available)::bigint AS priced_requests,
            COUNT(*) FILTER (WHERE NOT records.pricing_available)::bigint AS unpriced_requests,


            COUNT(*) FILTER (WHERE records.simulated)::bigint AS simulated_requests,
            COUNT(*) FILTER (WHERE records.usage_source = 'upstream_metadata')::bigint
                AS upstream_metadata_requests,
            COUNT(*) FILTER (WHERE records.dashboard_account)::bigint
                AS account_requests,
            COUNT(*) FILTER (
                WHERE records.dashboard_account
                  AND records.dashboard_external_billing_present
                  AND records.dashboard_external_pricing_available
            )::bigint AS account_priced_requests,
            COUNT(*) FILTER (
                WHERE records.dashboard_account
                  AND NOT (
                      records.dashboard_external_billing_present
                      AND records.dashboard_external_pricing_available
                  )
            )::bigint AS account_unpriced_requests,
            COUNT(*) FILTER (
                WHERE records.dashboard_external_billing_present
                  AND records.dashboard_external_cost_floor_applied
            )::bigint AS account_cost_floor_applied_requests,
            COALESCE(SUM(records.dashboard_external_raw_cost_usd), 0)::double precision
                AS account_raw_cost_usd,
            COALESCE(SUM(records.dashboard_external_effective_shaped_cost_usd), 0)::double precision
                AS account_shaped_cost_usd,
            COALESCE(SUM(records.dashboard_external_effective_uplifted_cost_usd), 0)::double precision
                AS account_uplifted_cost_usd,
            COALESCE(SUM(
                records.dashboard_external_effective_uplifted_cost_usd
                - records.dashboard_external_raw_cost_usd
            ), 0)::double precision AS account_profit_usd,
            COALESCE(SUM(records.dashboard_external_reported_cost_usd), 0)::double precision
                AS account_reported_cost_usd,
            COALESCE(SUM(records.dashboard_external_billable_cost_usd), 0)::double precision
                AS account_billable_cost_usd,
            COALESCE(SUM(records.dashboard_external_cost_floor_delta_usd), 0)::double precision
                AS account_cost_floor_delta_usd,
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
            SUM(segment_rows.priced_requests)::bigint AS priced_requests,
            SUM(segment_rows.unpriced_requests)::bigint AS unpriced_requests,

            SUM(segment_rows.simulated_requests)::bigint AS simulated_requests,
            SUM(segment_rows.upstream_metadata_requests)::bigint AS upstream_metadata_requests,
            SUM(segment_rows.account_requests)::bigint AS account_requests,
            SUM(segment_rows.account_priced_requests)::bigint
                AS account_priced_requests,
            SUM(segment_rows.account_unpriced_requests)::bigint
                AS account_unpriced_requests,
            SUM(segment_rows.account_cost_floor_applied_requests)::bigint
                AS account_cost_floor_applied_requests,
            SUM(segment_rows.account_raw_cost_usd)::double precision
                AS account_raw_cost_usd,
            SUM(segment_rows.account_shaped_cost_usd)::double precision
                AS account_shaped_cost_usd,
            SUM(segment_rows.account_uplifted_cost_usd)::double precision
                AS account_uplifted_cost_usd,
            SUM(segment_rows.account_profit_usd)::double precision
                AS account_profit_usd,
            SUM(segment_rows.account_reported_cost_usd)::double precision
                AS account_reported_cost_usd,
            SUM(segment_rows.account_billable_cost_usd)::double precision
                AS account_billable_cost_usd,
            SUM(segment_rows.account_cost_floor_delta_usd)::double precision
                AS account_cost_floor_delta_usd,
            SUM(segment_rows.duration_ms_sum)::numeric AS duration_ms_sum,
            SUM(segment_rows.duration_ms_count)::bigint AS duration_ms_count
        FROM dashboard_global_segment_rows segment_rows
        GROUP BY segment_rows.window_key, segment_rows.segment_hour
        ) "#,
    );
}

fn push_dashboard_account_segments_cte(builder: &mut QueryBuilder<'_, Postgres>) {
    builder.push(
        r#", dashboard_account_segment_rows AS (
        SELECT
            w.key AS window_key,
            date_trunc('hour', buckets.bucket_start AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'
                AS segment_hour,
            buckets.dimension_key AS pool_id,
            buckets.dimension_label AS pool_name,
            buckets.account_requests,
            buckets.account_priced_requests,
            buckets.account_unpriced_requests,
            buckets.account_cost_floor_applied_requests,
            buckets.account_raw_cost_usd,
            buckets.account_shaped_cost_usd,
            buckets.account_uplifted_cost_usd,
            buckets.account_profit_usd,
            buckets.account_reported_cost_usd,
            buckets.account_billable_cost_usd,
            buckets.account_cost_floor_delta_usd
        FROM window_bounds w
        JOIN usage_rollup_time_buckets buckets
          ON buckets.dimension = 'account'
         AND buckets.bucket_start >= w.full_from_at
         AND buckets.bucket_start < w.full_to_at

        UNION ALL

        SELECT
            records.window_key,
            records.boundary_hour AS segment_hour,
            records.dashboard_account_id AS pool_id,
            MAX(records.dashboard_account_name) AS pool_name,
            COUNT(*) FILTER (WHERE records.dashboard_account)::bigint
                AS account_requests,
            COUNT(*) FILTER (
                WHERE records.dashboard_account
                  AND records.dashboard_external_billing_present
                  AND records.dashboard_external_pricing_available
            )::bigint AS account_priced_requests,
            COUNT(*) FILTER (
                WHERE records.dashboard_account
                  AND NOT (
                      records.dashboard_external_billing_present
                      AND records.dashboard_external_pricing_available
                  )
            )::bigint AS account_unpriced_requests,
            COUNT(*) FILTER (
                WHERE records.dashboard_external_billing_present
                  AND records.dashboard_external_cost_floor_applied
            )::bigint AS account_cost_floor_applied_requests,
            COALESCE(SUM(records.dashboard_external_raw_cost_usd), 0)::double precision
                AS account_raw_cost_usd,
            COALESCE(SUM(records.dashboard_external_effective_shaped_cost_usd), 0)::double precision
                AS account_shaped_cost_usd,
            COALESCE(SUM(records.dashboard_external_effective_uplifted_cost_usd), 0)::double precision
                AS account_uplifted_cost_usd,
            COALESCE(SUM(
                records.dashboard_external_effective_uplifted_cost_usd
                - records.dashboard_external_raw_cost_usd
            ), 0)::double precision AS account_profit_usd,
            COALESCE(SUM(records.dashboard_external_reported_cost_usd), 0)::double precision
                AS account_reported_cost_usd,
            COALESCE(SUM(records.dashboard_external_billable_cost_usd), 0)::double precision
                AS account_billable_cost_usd,
            COALESCE(SUM(records.dashboard_external_cost_floor_delta_usd), 0)::double precision
                AS account_cost_floor_delta_usd
        FROM window_boundary_records records
        WHERE records.dashboard_account_id ~ '^[0-9]+$'
        GROUP BY
            records.window_key,
            records.boundary_hour,
            records.dashboard_account_id
        ), dashboard_account_segments AS (
        SELECT
            segment_rows.window_key,
            segment_rows.segment_hour,
            segment_rows.pool_id,
            MAX(segment_rows.pool_name) AS pool_name,
            SUM(segment_rows.account_requests)::bigint AS account_requests,
            SUM(segment_rows.account_priced_requests)::bigint
                AS account_priced_requests,
            SUM(segment_rows.account_unpriced_requests)::bigint
                AS account_unpriced_requests,
            SUM(segment_rows.account_cost_floor_applied_requests)::bigint
                AS account_cost_floor_applied_requests,
            SUM(segment_rows.account_raw_cost_usd)::double precision
                AS account_raw_cost_usd,
            SUM(segment_rows.account_shaped_cost_usd)::double precision
                AS account_shaped_cost_usd,
            SUM(segment_rows.account_uplifted_cost_usd)::double precision
                AS account_uplifted_cost_usd,
            SUM(segment_rows.account_profit_usd)::double precision
                AS account_profit_usd,
            SUM(segment_rows.account_reported_cost_usd)::double precision
                AS account_reported_cost_usd,
            SUM(segment_rows.account_billable_cost_usd)::double precision
                AS account_billable_cost_usd,
            SUM(segment_rows.account_cost_floor_delta_usd)::double precision
                AS account_cost_floor_delta_usd
        FROM dashboard_account_segment_rows segment_rows
        GROUP BY segment_rows.window_key, segment_rows.segment_hour, segment_rows.pool_id
        ) "#,
    );
}

fn row_i64_to_usize(row: &PgRow, column: &str) -> anyhow::Result<usize> {
    let value: i64 = row.try_get(column)?;
    Ok(value.max(0) as usize)
}

#[derive(Debug, Clone, Copy, Default)]
struct OverviewAccumulator {
    requests: i64,
    success_requests: i64,
    error_requests: i64,
    input_tokens: i64,
    uncached_input_tokens: i64,
    output_tokens: i64,
    cache_read_input_tokens: i64,
    cache_creation_input_tokens: i64,
    estimated_cost_usd: f64,
    original_cost_usd: f64,

    priced_requests: i64,
    unpriced_requests: i64,
    duration_ms_sum: i64,
    duration_ms_count: i64,
    account_cost_floor_applied_requests: i64,
    account_cost_usd: f64,
    account_raw_tokens: i64,
}

impl OverviewAccumulator {
    fn from_row(row: &PgRow) -> anyhow::Result<Self> {
        Ok(Self {
            requests: row.try_get("requests")?,
            success_requests: row.try_get("success_requests")?,
            error_requests: row.try_get("error_requests")?,
            input_tokens: row.try_get("total_input_tokens")?,
            uncached_input_tokens: row.try_get("total_compat_input_tokens")?,
            output_tokens: row.try_get("total_output_tokens")?,
            cache_read_input_tokens: row.try_get("total_cache_read_input_tokens")?,
            cache_creation_input_tokens: row.try_get("total_cache_creation_input_tokens")?,
            estimated_cost_usd: row.try_get("total_estimated_cost_usd")?,
            original_cost_usd: row.try_get("total_original_cost_usd")?,

            priced_requests: row.try_get("priced_requests")?,
            unpriced_requests: row.try_get("unpriced_requests")?,
            duration_ms_sum: row.try_get("duration_ms_sum")?,
            duration_ms_count: row.try_get("duration_ms_count")?,
            account_cost_floor_applied_requests: row
                .try_get("account_cost_floor_applied_requests")?,
            account_cost_usd: row.try_get("account_cost_usd")?,
            account_raw_tokens: row.try_get("account_raw_tokens")?,
        })
    }

    fn add(&mut self, other: Self) {
        self.requests += other.requests;
        self.success_requests += other.success_requests;
        self.error_requests += other.error_requests;
        self.input_tokens += other.input_tokens;
        self.uncached_input_tokens += other.uncached_input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_input_tokens += other.cache_read_input_tokens;
        self.cache_creation_input_tokens += other.cache_creation_input_tokens;
        self.estimated_cost_usd += other.estimated_cost_usd;
        self.original_cost_usd += other.original_cost_usd;
        self.priced_requests += other.priced_requests;
        self.unpriced_requests += other.unpriced_requests;
        self.duration_ms_sum += other.duration_ms_sum;
        self.duration_ms_count += other.duration_ms_count;
        self.account_cost_floor_applied_requests += other.account_cost_floor_applied_requests;
        self.account_cost_usd += other.account_cost_usd;
        self.account_raw_tokens += other.account_raw_tokens;
    }

    fn to_metrics(self) -> UsageOverviewMetrics {
        let requests = self.requests.max(0) as usize;
        let error_requests = self.error_requests.max(0) as usize;
        UsageOverviewMetrics {
            requests,
            success_requests: self.success_requests.max(0) as usize,
            error_requests,
            error_rate: usage_ratio(error_requests, requests),
            input_tokens: self.input_tokens.max(0),
            uncached_input_tokens: self.uncached_input_tokens.max(0),
            output_tokens: self.output_tokens.max(0),
            cache_read_input_tokens: self.cache_read_input_tokens.max(0),
            cache_creation_input_tokens: self.cache_creation_input_tokens.max(0),
            estimated_cost_usd: self.estimated_cost_usd.max(0.0),
            original_cost_usd: self.original_cost_usd.max(0.0),

            priced_requests: self.priced_requests.max(0) as usize,
            unpriced_requests: self.unpriced_requests.max(0) as usize,
            average_duration_ms: if self.duration_ms_count > 0 {
                self.duration_ms_sum.max(0) as f64 / self.duration_ms_count as f64
            } else {
                0.0
            },
        }
    }

    fn to_series_metrics(self) -> UsageOverviewSeriesMetrics {
        let metrics = self.to_metrics();
        UsageOverviewSeriesMetrics {
            requests: metrics.requests,
            success_requests: metrics.success_requests,
            error_requests: metrics.error_requests,
            average_duration_ms: metrics.average_duration_ms,
            input_tokens: metrics.input_tokens,
            uncached_input_tokens: metrics.uncached_input_tokens,
            output_tokens: metrics.output_tokens,
            cache_read_input_tokens: metrics.cache_read_input_tokens,
            cache_creation_input_tokens: metrics.cache_creation_input_tokens,
            estimated_cost_usd: metrics.estimated_cost_usd,
            original_cost_usd: metrics.original_cost_usd,

            raw_cost_usd: 0.0,
            billable_cost_usd: 0.0,
            profit_usd: 0.0,
        }
    }
}

/// External-pool cost columns carried by every rollup row. Only account
/// records contribute non-zero values, so local-route cells stay at zero.
#[derive(Debug, Clone, Copy, Default)]
struct OverviewExternalCosts {
    raw_cost_usd: f64,
    billable_cost_usd: f64,
    profit_usd: f64,
}

impl OverviewExternalCosts {
    fn from_row(row: &PgRow) -> anyhow::Result<Self> {
        Ok(Self {
            raw_cost_usd: row.try_get("account_raw_cost_usd")?,
            billable_cost_usd: row.try_get("account_billable_cost_usd")?,
            profit_usd: row.try_get("account_profit_usd")?,
        })
    }

    fn add(&mut self, other: Self) {
        self.raw_cost_usd += other.raw_cost_usd;
        self.billable_cost_usd += other.billable_cost_usd;
        self.profit_usd += other.profit_usd;
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct OverviewSeriesCell {
    metrics: OverviewAccumulator,
    costs: OverviewExternalCosts,
}

impl OverviewSeriesCell {
    fn add(&mut self, metrics: OverviewAccumulator, costs: OverviewExternalCosts) {
        self.metrics.add(metrics);
        self.costs.add(costs);
    }

    fn to_series_metrics(self) -> UsageOverviewSeriesMetrics {
        UsageOverviewSeriesMetrics {
            raw_cost_usd: self.costs.raw_cost_usd.max(0.0),
            billable_cost_usd: self.costs.billable_cost_usd.max(0.0),
            profit_usd: self.costs.profit_usd,
            ..self.metrics.to_series_metrics()
        }
    }
}

#[derive(Debug, Clone, Default)]
struct OverviewSeriesAccumulator {
    all: OverviewSeriesCell,
    external: OverviewSeriesCell,
    accounts: HashMap<String, OverviewSeriesCell>,
    routes: HashMap<String, OverviewSeriesCell>,
}

#[derive(Debug, Default)]
struct OverviewSeriesGroupMember {
    label: Option<String>,
    weight: f64,
    requests: i64,
}

/// Collects the members of one breakdown across the whole range so every
/// bucket can be emitted in the same order and labels are sent once.
#[derive(Debug, Default)]
struct OverviewSeriesGroupAccumulator {
    members: HashMap<String, OverviewSeriesGroupMember>,
}

impl OverviewSeriesGroupAccumulator {
    fn add(&mut self, key: &str, label: Option<&str>, weight: f64, requests: i64) {
        let member = self.members.entry(key.to_string()).or_default();
        if let Some(label) = label.map(str::trim).filter(|value| !value.is_empty()) {
            member.label = Some(label.to_string());
        }
        member.weight += weight;
        member.requests += requests;
    }

    fn finish(self, default_label: impl Fn(&str) -> String) -> Vec<UsageOverviewSeriesGroup> {
        let mut members = self
            .members
            .into_iter()
            .filter(|(_, member)| member.requests > 0)
            .collect::<Vec<_>>();
        members.sort_by(|(left_key, left), (right_key, right)| {
            right
                .weight
                .total_cmp(&left.weight)
                .then_with(|| right.requests.cmp(&left.requests))
                .then_with(|| left_key.cmp(right_key))
        });
        members
            .into_iter()
            .map(|(key, member)| UsageOverviewSeriesGroup {
                label: member.label.unwrap_or_else(|| default_label(&key)),
                key,
            })
            .collect()
    }
}

#[derive(Debug, Default)]
struct OverviewSeriesGroupsAccumulator {
    accounts: OverviewSeriesGroupAccumulator,
    routes: OverviewSeriesGroupAccumulator,
}

impl OverviewSeriesGroupsAccumulator {
    fn finish(self) -> UsageOverviewSeriesGroups {
        UsageOverviewSeriesGroups {
            accounts: self.accounts.finish(|key| format!("#{key}")),
            routes: self.routes.finish(str::to_string),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct OverviewPoolAccumulator {
    metrics: OverviewAccumulator,
    /// `account_attempt` 维度：该账号的每次尝试（含被切走的失败）。
    attempts: OverviewAccumulator,
    label: Option<String>,
    /// 账号成本（上游原始成本 × 账号倍率）。
    account_cost_usd: f64,
    raw_cost_usd: f64,
    shaped_cost_usd: f64,
    uplifted_cost_usd: f64,
    reported_cost_usd: f64,
    billable_cost_usd: f64,
    profit_usd: f64,
    cost_floor_delta_usd: f64,
    cost_floor_applied_requests: usize,
}

#[derive(Debug, Clone, Default)]
struct OverviewRankAccumulator {
    label: String,
    requests: usize,
    success_requests: usize,
    error_requests: usize,
    estimated_cost_usd: f64,
    original_cost_usd: f64,
    account_cost_usd: f64,
    cache_read_input_tokens: i64,
    cache_creation_input_tokens: i64,
    uncached_input_tokens: i64,
    duration_ms_sum: i64,
    duration_ms_count: i64,
}

impl OverviewRankAccumulator {
    fn add(&mut self, metrics: OverviewAccumulator) {
        self.requests = self
            .requests
            .saturating_add(metrics.requests.max(0) as usize);
        self.success_requests = self
            .success_requests
            .saturating_add(metrics.success_requests.max(0) as usize);
        self.error_requests = self
            .error_requests
            .saturating_add(metrics.error_requests.max(0) as usize);
        self.estimated_cost_usd += metrics.estimated_cost_usd.max(0.0);
        self.original_cost_usd += metrics.original_cost_usd.max(0.0);
        self.account_cost_usd += metrics.account_cost_usd.max(0.0);
        self.cache_read_input_tokens += metrics.cache_read_input_tokens.max(0);
        self.cache_creation_input_tokens += metrics.cache_creation_input_tokens.max(0);
        self.uncached_input_tokens += metrics.uncached_input_tokens.max(0);
        self.duration_ms_sum += metrics.duration_ms_sum.max(0);
        self.duration_ms_count += metrics.duration_ms_count.max(0);
    }

    fn average_duration_ms(&self) -> f64 {
        if self.duration_ms_count > 0 {
            self.duration_ms_sum as f64 / self.duration_ms_count as f64
        } else {
            0.0
        }
    }
}

fn overview_series_key(spec: &UsageOverviewSpec, bucket_start: DateTime<Utc>) -> String {
    if spec.granularity == "hour" {
        format!("hour:{}", bucket_start.to_rfc3339())
    } else {
        format!(
            "day:{}",
            bucket_start.with_timezone(&spec.offset).date_naive()
        )
    }
}

fn overview_series_slots(spec: &UsageOverviewSpec) -> Vec<(String, DateTime<Utc>, String)> {
    if spec.granularity == "hour" {
        let mut slots = Vec::new();
        let mut cursor = spec.from;
        while cursor < spec.to {
            slots.push((
                overview_series_key(spec, cursor),
                cursor,
                cursor
                    .with_timezone(&spec.offset)
                    .format("%m-%d %H:00")
                    .to_string(),
            ));
            cursor += ChronoDuration::hours(1);
        }
        return slots;
    }

    let start = spec.from.with_timezone(&spec.offset).date_naive();
    let end = (spec.to - ChronoDuration::seconds(1))
        .with_timezone(&spec.offset)
        .date_naive();
    let mut date = start;
    let mut slots = Vec::new();
    while date <= end {
        let local_midnight = spec
            .offset
            .with_ymd_and_hms(date.year(), date.month(), date.day(), 0, 0, 0)
            .single()
            .expect("fixed offset local midnight")
            .with_timezone(&Utc);
        slots.push((
            format!("day:{}", date),
            local_midnight,
            date.format("%m-%d").to_string(),
        ));
        date += ChronoDuration::days(1);
    }
    slots
}

fn overview_series_breakdown(
    order: &[UsageOverviewSeriesGroup],
    cells: &HashMap<String, OverviewSeriesCell>,
) -> Vec<UsageOverviewSeriesBreakdown> {
    // Absent members are omitted; the client treats a missing key as zero.
    order
        .iter()
        .filter_map(|group| {
            cells
                .get(&group.key)
                .map(|cell| UsageOverviewSeriesBreakdown {
                    key: group.key.clone(),
                    metrics: cell.to_series_metrics(),
                })
        })
        .collect()
}

fn overview_series_points(
    spec: &UsageOverviewSpec,
    mut series: HashMap<String, OverviewSeriesAccumulator>,
    groups: &UsageOverviewSeriesGroups,
) -> Vec<UsageOverviewSeriesPoint> {
    overview_series_slots(spec)
        .into_iter()
        .map(|(key, bucket_start, label)| {
            let values = series.remove(&key).unwrap_or_default();
            UsageOverviewSeriesPoint {
                bucket_start: bucket_start.to_rfc3339(),
                label,
                all: values.all.to_series_metrics(),
                external: values.external.to_series_metrics(),
                accounts: overview_series_breakdown(&groups.accounts, &values.accounts),
                routes: overview_series_breakdown(&groups.routes, &values.routes),
            }
        })
        .collect()
}

fn ranked_overview_rows(
    values: HashMap<String, OverviewRankAccumulator>,
) -> Vec<UsageOverviewRankRow> {
    let mut rows = values
        .into_iter()
        .map(|(key, value)| UsageOverviewRankRow {
            label: if value.label.is_empty() {
                key.clone()
            } else {
                value.label.clone()
            },
            key,
            requests: value.requests,
            error_requests: value.error_requests,
            estimated_cost_usd: value.estimated_cost_usd,
            success_requests: value.success_requests,
            cache_read_input_tokens: value.cache_read_input_tokens,
            cache_creation_input_tokens: value.cache_creation_input_tokens,
            uncached_input_tokens: value.uncached_input_tokens,
            original_cost_usd: value.original_cost_usd,
            account_cost_usd: value.account_cost_usd,
            average_duration_ms: value.average_duration_ms(),
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .requests
            .cmp(&left.requests)
            .then_with(|| right.estimated_cost_usd.total_cmp(&left.estimated_cost_usd))
            .then_with(|| left.key.cmp(&right.key))
    });
    rows
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

            priced_requests: row_i64_to_usize(&row, "priced_requests")?,
            unpriced_requests: row_i64_to_usize(&row, "unpriced_requests")?,
            average_duration_ms: row.try_get("average_duration_ms")?,
            p95_duration_ms: p95_duration_ms.max(0) as u64,

            simulated_requests: row_i64_to_usize(&row, "simulated_requests")?,
            upstream_metadata_requests: row_i64_to_usize(&row, "upstream_metadata_requests")?,
            account_billing: UsageAccountBillingSummary {
                requests: row_i64_to_usize(&row, "account_requests")?,
                priced_requests: row_i64_to_usize(&row, "account_priced_requests")?,
                unpriced_requests: row_i64_to_usize(&row, "account_unpriced_requests")?,
                cost_floor_applied_requests: row_i64_to_usize(
                    &row,
                    "account_cost_floor_applied_requests",
                )?,
                raw_cost_usd: row.try_get("account_raw_cost_usd")?,
                shaped_cost_usd: row.try_get("account_shaped_cost_usd")?,
                uplifted_cost_usd: row.try_get("account_uplifted_cost_usd")?,
                profit_usd: row.try_get("account_profit_usd")?,
                reported_cost_usd: row.try_get("account_reported_cost_usd")?,
                billable_cost_usd: row.try_get("account_billable_cost_usd")?,
                cost_floor_delta_usd: row.try_get("account_cost_floor_delta_usd")?,
            },
            account_billing_by_pool: Vec::new(),
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

            priced_requests: 0,
            unpriced_requests: 0,
            average_duration_ms: 0.0,
            p95_duration_ms: 0,

            simulated_requests: 0,
            upstream_metadata_requests: 0,
            account_billing: UsageAccountBillingSummary::default(),
            account_billing_by_pool: Vec::new(),
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

fn account_eligibility_from_row(row: &PgRow) -> anyhow::Result<AccountEligibility> {
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
    AccountAuthType::parse_known(&auth_type).ok_or_else(|| anyhow::anyhow!("auth_type 值无效"))?;
    let max_concurrent_requests: i32 = row.try_get("max_concurrent_requests")?;
    if max_concurrent_requests <= 0 {
        anyhow::bail!("max_concurrent_requests 必须为正数");
    }
    let _raw_usage_source = RawUsageSource::parse(&row.try_get::<String, _>("raw_usage_source")?)?;
    let usage_projection_mode: String = row.try_get("usage_projection_mode")?;
    UsageProjectionMode::parse_known(&usage_projection_mode)
        .ok_or_else(|| anyhow::anyhow!("usage_projection_mode 值无效"))?;
    let stream_response_mode: Option<String> = row.try_get("stream_response_mode")?;
    if stream_response_mode
        .as_deref()
        .is_some_and(|value| StreamResponseMode::parse_known(value).is_none())
    {
        anyhow::bail!("stream_response_mode 值无效");
    }
    let auto_disable_policy: String = row.try_get("auto_disable_policy")?;
    AutoDisablePolicy::parse_known(&auto_disable_policy)
        .ok_or_else(|| anyhow::anyhow!("auto_disable_policy 值无效"))?;
    let pre_output_stream_retry_mode: String = row.try_get("pre_output_stream_retry_mode")?;
    StreamRetryMode::parse_known(&pre_output_stream_retry_mode)
        .ok_or_else(|| anyhow::anyhow!("pre_output_stream_retry_mode 值无效"))?;
    let header_profile: String = row.try_get("header_profile")?;
    AccountHeaderProfile::parse_known(&header_profile)
        .ok_or_else(|| anyhow::anyhow!("header_profile 值无效"))?;
    row.try_get::<bool, _>("append_beta_query")?;
    let header_overrides_value: serde_json::Value = row
        .try_get("header_overrides")
        .map_err(|_| anyhow::anyhow!("header_overrides 字段类型无效"))?;
    let header_overrides =
        serde_json::from_value::<HashMap<String, String>>(header_overrides_value)
            .map_err(|_| anyhow::anyhow!("header_overrides 必须是对象"))?;
    normalize_account_header_overrides(header_overrides)?;
    let wire_profile: String = row.try_get("wire_profile")?;
    AccountWireProfile::parse_known(&wire_profile)
        .ok_or_else(|| anyhow::anyhow!("wire_profile 值无效"))?;
    let tls_profile: String = row.try_get("tls_profile")?;
    AccountTlsProfile::parse_known(&tls_profile)
        .ok_or_else(|| anyhow::anyhow!("tls_profile 值无效"))?;
    let route_mode: String = row.try_get("route_mode")?;
    let route_mode = RouteRuleMode::parse_known(&route_mode)
        .ok_or_else(|| anyhow::anyhow!("route_mode 值无效"))?;
    let route_rules_value: serde_json::Value = row
        .try_get("route_rules")
        .map_err(|_| anyhow::anyhow!("route_rules 字段类型无效"))?;
    let route_rules = decode_account_route_rules(route_rules_value, true)?;
    let model_mapping_mode: String = row.try_get("model_mapping_mode")?;
    ModelMappingMode::parse_known(&model_mapping_mode)
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
    Ok(AccountEligibility {
        id: row.try_get::<i64, _>("id")?.max(0) as u64,
        enabled: row.try_get("enabled")?,
        auto_disabled: row.try_get("auto_disabled")?,
        auto_disabled_until: row.try_get("auto_disabled_until")?,
        model_filter_mode: crate::model::model_support::ModelFilterMode::parse(
            &row.try_get::<String, _>("model_filter_mode")?,
        )?,
        model_mapping_mode: ModelMappingMode::parse_known(
            &row.try_get::<String, _>("model_mapping_mode")?,
        )
        .ok_or_else(|| anyhow::anyhow!("invalid model mapping mode"))?,
        model_mapping_rules: Arc::new(model_mapping_rules),
        supported_models: Arc::new(supported_models),
        route_mode,
        route_rules: Arc::new(route_rules),
    })
}

fn decode_account_route_rules(
    value: serde_json::Value,
    strict_dispatch: bool,
) -> anyhow::Result<Vec<String>> {
    let rules = if strict_dispatch {
        serde_json::from_value::<Vec<String>>(value)
            .map_err(|_| anyhow::anyhow!("route_rules 必须是字符串数组"))?
    } else {
        serde_json::from_value::<Vec<String>>(value).unwrap_or_default()
    };
    validate_account_route_rules(&rules)?;
    Ok(normalize_route_rules(&rules))
}

fn validate_account_route_rules(rules: &[String]) -> anyhow::Result<()> {
    if rules.len() > 200 {
        anyhow::bail!("route_rules 不能超过 200 条");
    }
    if rules.iter().any(|rule| rule.len() > 256) {
        anyhow::bail!("route_rules 单条长度不能超过 256");
    }
    Ok(())
}

fn account_from_row(row: PgRow, mask_secrets: bool) -> anyhow::Result<Account> {
    account_from_row_with_policy(row, mask_secrets, false)
}

fn account_from_row_with_policy(
    row: PgRow,
    mask_secrets: bool,
    strict_dispatch: bool,
) -> anyhow::Result<Account> {
    let id: i64 = row.try_get("id")?;
    let revision: i64 = row.try_get("revision")?;
    let api_key: String = row.try_get("api_key")?;
    let auth_type: String = row.try_get("auth_type")?;
    let raw_usage_source = RawUsageSource::parse(&row.try_get::<String, _>("raw_usage_source")?)?;
    let usage_projection_mode: String = row.try_get("usage_projection_mode")?;
    let stream_response_mode: Option<String> = if strict_dispatch {
        row.try_get("stream_response_mode")?
    } else {
        row.try_get("stream_response_mode").unwrap_or(None)
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
        normalize_account_header_overrides(overrides)?
    } else {
        serde_json::from_value::<HashMap<String, String>>(header_overrides_value)
            .ok()
            .and_then(|overrides| normalize_account_header_overrides(overrides).ok())
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
    let route_rules = decode_account_route_rules(route_rules_value, strict_dispatch)?;
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
        AccountAuthType::parse_known(&auth_type)
            .ok_or_else(|| anyhow::anyhow!("auth_type 值无效"))?
    } else {
        AccountAuthType::parse(&auth_type)
    };
    let usage_projection_mode = if strict_dispatch {
        UsageProjectionMode::parse_known(&usage_projection_mode)
            .ok_or_else(|| anyhow::anyhow!("usage_projection_mode 值无效"))?
    } else {
        UsageProjectionMode::parse(&usage_projection_mode)
    };
    let stream_response_mode = match stream_response_mode.as_deref() {
        Some(value) if strict_dispatch => Some(
            StreamResponseMode::parse_known(value)
                .ok_or_else(|| anyhow::anyhow!("stream_response_mode 值无效"))?,
        ),
        Some(value) => Some(StreamResponseMode::parse(value)),
        None => None,
    };
    let auto_disable_policy = if strict_dispatch {
        AutoDisablePolicy::parse_known(&auto_disable_policy)
            .ok_or_else(|| anyhow::anyhow!("auto_disable_policy 值无效"))?
    } else {
        AutoDisablePolicy::parse(&auto_disable_policy)
    };
    let pre_output_stream_retry_mode = if strict_dispatch {
        StreamRetryMode::parse_known(&pre_output_stream_retry_mode)
            .ok_or_else(|| anyhow::anyhow!("pre_output_stream_retry_mode 值无效"))?
    } else {
        StreamRetryMode::parse(&pre_output_stream_retry_mode)
    };
    let header_profile = if strict_dispatch {
        AccountHeaderProfile::parse_known(&header_profile)
            .ok_or_else(|| anyhow::anyhow!("header_profile 值无效"))?
    } else {
        AccountHeaderProfile::parse(&header_profile)
    };
    let wire_profile = if strict_dispatch {
        AccountWireProfile::parse_known(&wire_profile)
            .ok_or_else(|| anyhow::anyhow!("wire_profile 值无效"))?
    } else {
        AccountWireProfile::parse(&wire_profile)
    };
    let tls_profile = if strict_dispatch {
        AccountTlsProfile::parse_known(&tls_profile)
            .ok_or_else(|| anyhow::anyhow!("tls_profile 值无效"))?
    } else {
        AccountTlsProfile::parse(&tls_profile)
    };
    let route_mode = if strict_dispatch {
        RouteRuleMode::parse_known(&route_mode)
            .ok_or_else(|| anyhow::anyhow!("route_mode 值无效"))?
    } else {
        RouteRuleMode::parse(&route_mode)
    };
    let model_mapping_mode = if strict_dispatch {
        ModelMappingMode::parse_known(&model_mapping_mode)
            .ok_or_else(|| anyhow::anyhow!("model_mapping_mode 值无效"))?
    } else {
        ModelMappingMode::parse(&model_mapping_mode)
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
    let proxy_url: Option<String> = row.try_get("proxy_url")?;
    let cost_multiplier: f64 = row.try_get("cost_multiplier")?;
    let overrides = serde_json::from_value(row.try_get("overrides")?)?;
    if strict_dispatch {
        crate::account::validate_account_fields(proxy_url.as_deref(), cost_multiplier, &overrides)?;
    }
    Ok(Account {
        tags: row.try_get("tags")?,
        proxy_url,
        cost_multiplier,
        overrides,
        id: id.max(0) as u64,
        revision: revision.max(1) as u64,
        name: row.try_get("name")?,
        base_url: row.try_get("base_url")?,
        api_key: (!mask_secrets).then_some(api_key.clone()),
        masked_api_key: Some(mask_account_key(&api_key)),
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
        raw_usage_source,
        stream_response_mode,
        auto_disable_policy,
        pre_output_stream_retry_mode,
        auto_disabled: row.try_get("auto_disabled")?,
        auto_disabled_reason: row.try_get("auto_disabled_reason")?,
        auto_disabled_at: row.try_get("auto_disabled_at")?,
        auto_disabled_until: row.try_get("auto_disabled_until")?,
        auto_disabled_last_error: row.try_get("auto_disabled_last_error")?,
        model_mapping_mode,
        model_mapping_require_match,
        model_mapping_rules: normalize_account_model_mapping_rules(model_mapping_rules),
        model_filter_mode: crate::model::model_support::ModelFilterMode::parse(
            &row.try_get::<String, _>("model_filter_mode")?,
        )?,
        supported_models,
        route_mode,
        route_rules,
        notes: row.try_get("notes")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn validate_account_input(
    name: &str,
    base_url: &str,
    max_concurrent_requests: u32,
) -> anyhow::Result<()> {
    if name.trim().is_empty() {
        anyhow::bail!("账号名称不能为空");
    }
    if base_url.trim().is_empty() {
        anyhow::bail!("账号 baseUrl 不能为空");
    }
    let parsed = url::Url::parse(base_url.trim())?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        anyhow::bail!("账号 baseUrl 只支持 http/https");
    }
    if max_concurrent_requests == 0 {
        anyhow::bail!("账号 maxConcurrentRequests 必须大于 0");
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
        UsageRouteKind::Account => "account",
    }
}

/// One stored route scenario: a named snapshot of route rules (see admin route scenarios).
#[derive(Debug, Clone)]
pub struct RouteScenarioRow {
    pub id: u64,
    pub name: String,
    pub description: Option<String>,
    pub content: serde_json::Value,
    pub revision: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn route_scenario_from_row(row: sqlx::postgres::PgRow) -> anyhow::Result<RouteScenarioRow> {
    Ok(RouteScenarioRow {
        id: row.try_get::<i64, _>("id")? as u64,
        name: row.try_get("name")?,
        description: row.try_get("description")?,
        content: row.try_get("content")?,
        revision: row.try_get::<i64, _>("revision")?.max(0) as u64,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
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


CREATE TABLE IF NOT EXISTS accounts (
    tags TEXT[] NOT NULL DEFAULT '{}',
    proxy_url TEXT,
    cost_multiplier DOUBLE PRECISION NOT NULL DEFAULT 1,
    overrides JSONB NOT NULL DEFAULT '{}'::jsonb,
    id BIGSERIAL PRIMARY KEY,
    revision BIGINT NOT NULL DEFAULT 1,
    name TEXT NOT NULL,
    base_url TEXT NOT NULL,
    api_key TEXT NOT NULL,
    auth_type TEXT NOT NULL DEFAULT 'bearer',
    enabled BOOLEAN NOT NULL DEFAULT true,
    priority INTEGER NOT NULL DEFAULT 100,
    max_concurrent_requests INTEGER NOT NULL DEFAULT 10,
    usage_projection_mode TEXT NOT NULL DEFAULT 'current_path_policy',
    stream_response_mode TEXT,
    raw_usage_source TEXT NOT NULL DEFAULT 'legacy_max' CHECK (raw_usage_source IN ('legacy_max', 'upstream_last', 'upstream_last_max_cost')),
    skip_non_stream_usage_projection BOOLEAN NOT NULL DEFAULT false,
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
    normalize_model_version_dots BOOLEAN NOT NULL DEFAULT false,
    model_mapping_mode TEXT NOT NULL DEFAULT 'passthrough_mapping',
    model_mapping_require_match BOOLEAN NOT NULL DEFAULT false,
    model_mapping_rules JSONB NOT NULL DEFAULT '[]'::jsonb,
    model_filter_mode TEXT NOT NULL DEFAULT 'allowlist' CHECK (model_filter_mode IN ('allowlist','denylist')),
    supported_models JSONB NOT NULL DEFAULT '[]'::jsonb,
    route_mode TEXT NOT NULL DEFAULT 'allow_all',
    route_rules JSONB NOT NULL DEFAULT '[]'::jsonb,
    notes TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at TIMESTAMPTZ
);

ALTER TABLE accounts ADD COLUMN IF NOT EXISTS model_filter_mode TEXT NOT NULL DEFAULT 'allowlist' CHECK (model_filter_mode IN ('allowlist','denylist'));

ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS revision BIGINT NOT NULL DEFAULT 1;

ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS auth_type TEXT NOT NULL DEFAULT 'bearer';

ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS enabled BOOLEAN NOT NULL DEFAULT true;

ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS priority INTEGER NOT NULL DEFAULT 100;

ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS max_concurrent_requests INTEGER NOT NULL DEFAULT 10;

ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS usage_projection_mode TEXT NOT NULL DEFAULT 'current_path_policy';

ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS stream_response_mode TEXT;

ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS raw_usage_source TEXT NOT NULL DEFAULT 'legacy_max' CHECK (raw_usage_source IN ('legacy_max', 'upstream_last', 'upstream_last_max_cost'));
ALTER TABLE accounts DROP CONSTRAINT IF EXISTS accounts_raw_usage_source_check;
ALTER TABLE accounts ADD CONSTRAINT accounts_raw_usage_source_check
    CHECK (raw_usage_source IN ('legacy_max', 'upstream_last', 'upstream_last_max_cost'));


ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS skip_non_stream_usage_projection BOOLEAN NOT NULL DEFAULT false;



ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS auto_disable_policy TEXT NOT NULL DEFAULT 'inherit';

ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS pre_output_stream_retry_mode TEXT NOT NULL DEFAULT 'inherit';

ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS header_profile TEXT NOT NULL DEFAULT 'generic';

ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS append_beta_query BOOLEAN NOT NULL DEFAULT false;

ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS header_overrides JSONB NOT NULL DEFAULT '{}'::jsonb;

ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS wire_profile TEXT NOT NULL DEFAULT 'default';

ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS tls_profile TEXT NOT NULL DEFAULT 'default';

ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS auto_disabled BOOLEAN NOT NULL DEFAULT false;

ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS auto_disabled_reason TEXT;

ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS auto_disabled_at TIMESTAMPTZ;

ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS auto_disabled_until TIMESTAMPTZ;

ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS auto_disabled_last_error TEXT;


ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS normalize_model_version_dots BOOLEAN NOT NULL DEFAULT false;

ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS model_mapping_mode TEXT NOT NULL DEFAULT 'passthrough_mapping';

ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS model_mapping_require_match BOOLEAN NOT NULL DEFAULT false;

ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS model_mapping_rules JSONB NOT NULL DEFAULT '[]'::jsonb;

ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS supported_models JSONB NOT NULL DEFAULT '[]'::jsonb;

ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS route_mode TEXT NOT NULL DEFAULT 'allow_all';

ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS route_rules JSONB NOT NULL DEFAULT '[]'::jsonb;

ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS notes TEXT;

ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS created_at TIMESTAMPTZ NOT NULL DEFAULT now();

ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT now();

ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS deleted_at TIMESTAMPTZ;

CREATE INDEX IF NOT EXISTS idx_accounts_active_priority
    ON accounts (priority ASC, id ASC)
    WHERE deleted_at IS NULL AND enabled = true;

CREATE INDEX IF NOT EXISTS idx_accounts_auto_disabled
    ON accounts (auto_disabled, auto_disabled_until)
    WHERE deleted_at IS NULL;

CREATE TABLE IF NOT EXISTS usage_records (
    id TEXT PRIMARY KEY,
    created_at TIMESTAMPTZ NOT NULL,
    endpoint TEXT NOT NULL,
    stream BOOLEAN NOT NULL,
    model TEXT NOT NULL,
    conversation_id TEXT,
    account_id BIGINT,
    account_label TEXT,
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
    account_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    pricing_available BOOLEAN NOT NULL DEFAULT false,
    pricing_model TEXT,
    duration_ms BIGINT NOT NULL DEFAULT 0,
    simulated BOOLEAN NOT NULL DEFAULT false,
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
    ADD COLUMN IF NOT EXISTS account_id BIGINT,
    ADD COLUMN IF NOT EXISTS account_label TEXT,
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
    ADD COLUMN IF NOT EXISTS account_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS pricing_available BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS pricing_model TEXT,
    ADD COLUMN IF NOT EXISTS duration_ms BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS simulated BOOLEAN NOT NULL DEFAULT false,
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
    include_summary BOOLEAN NOT NULL DEFAULT false,
    cutoff_at TIMESTAMPTZ NOT NULL,
    batch_size INTEGER NOT NULL CHECK (batch_size > 0 AND batch_size <= 10000),
    max_batches INTEGER NOT NULL CHECK (max_batches > 0 AND max_batches <= 10000),
    max_rows BIGINT CHECK (max_rows IS NULL OR max_rows >= 0),
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


    total_input_tokens BIGINT NOT NULL DEFAULT 0,
    billable_input_tokens BIGINT NOT NULL DEFAULT 0,
    total_compat_input_tokens BIGINT NOT NULL DEFAULT 0,
    total_output_tokens BIGINT NOT NULL DEFAULT 0,
    total_cache_read_input_tokens BIGINT NOT NULL DEFAULT 0,
    total_cache_creation_input_tokens BIGINT NOT NULL DEFAULT 0,
    local_prompt_cache_input_tokens BIGINT NOT NULL DEFAULT 0,
    local_prompt_cache_read_input_tokens BIGINT NOT NULL DEFAULT 0,
    local_prompt_cache_creation_input_tokens BIGINT NOT NULL DEFAULT 0,
    total_estimated_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    total_original_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,

    account_requests BIGINT NOT NULL DEFAULT 0,
    account_priced_requests BIGINT NOT NULL DEFAULT 0,
    account_unpriced_requests BIGINT NOT NULL DEFAULT 0,
    account_cost_floor_applied_requests BIGINT NOT NULL DEFAULT 0,
    account_raw_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    account_shaped_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    account_uplifted_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    account_profit_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    account_reported_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    account_billable_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    account_cost_floor_delta_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    duration_ms_sum BIGINT NOT NULL DEFAULT 0,
    duration_ms_count BIGINT NOT NULL DEFAULT 0,
    duration_ms_max BIGINT NOT NULL DEFAULT 0,
    account_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    account_raw_tokens BIGINT NOT NULL DEFAULT 0,
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


    ADD COLUMN IF NOT EXISTS total_input_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS billable_input_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS total_compat_input_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS total_output_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS total_cache_read_input_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS total_cache_creation_input_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS local_prompt_cache_input_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS local_prompt_cache_read_input_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS local_prompt_cache_creation_input_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS total_estimated_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS total_original_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,

    ADD COLUMN IF NOT EXISTS account_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS account_priced_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS account_unpriced_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS account_cost_floor_applied_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS account_raw_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS account_shaped_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS account_uplifted_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS account_profit_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS account_reported_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS account_billable_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS account_cost_floor_delta_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS duration_ms_sum BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS duration_ms_count BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS duration_ms_max BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS account_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS account_raw_tokens BIGINT NOT NULL DEFAULT 0,
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


    total_input_tokens BIGINT NOT NULL DEFAULT 0,
    billable_input_tokens BIGINT NOT NULL DEFAULT 0,
    total_compat_input_tokens BIGINT NOT NULL DEFAULT 0,
    total_output_tokens BIGINT NOT NULL DEFAULT 0,
    total_cache_read_input_tokens BIGINT NOT NULL DEFAULT 0,
    total_cache_creation_input_tokens BIGINT NOT NULL DEFAULT 0,
    local_prompt_cache_input_tokens BIGINT NOT NULL DEFAULT 0,
    local_prompt_cache_read_input_tokens BIGINT NOT NULL DEFAULT 0,
    local_prompt_cache_creation_input_tokens BIGINT NOT NULL DEFAULT 0,
    total_estimated_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    total_original_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,

    account_requests BIGINT NOT NULL DEFAULT 0,
    account_priced_requests BIGINT NOT NULL DEFAULT 0,
    account_unpriced_requests BIGINT NOT NULL DEFAULT 0,
    account_cost_floor_applied_requests BIGINT NOT NULL DEFAULT 0,
    account_raw_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    account_shaped_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    account_uplifted_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    account_profit_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    account_reported_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    account_billable_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    account_cost_floor_delta_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    duration_ms_sum BIGINT NOT NULL DEFAULT 0,
    duration_ms_count BIGINT NOT NULL DEFAULT 0,
    duration_ms_max BIGINT NOT NULL DEFAULT 0,
    account_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    account_raw_tokens BIGINT NOT NULL DEFAULT 0,
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


    ADD COLUMN IF NOT EXISTS total_input_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS billable_input_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS total_compat_input_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS total_output_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS total_cache_read_input_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS total_cache_creation_input_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS local_prompt_cache_input_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS local_prompt_cache_read_input_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS local_prompt_cache_creation_input_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS total_estimated_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS total_original_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,

    ADD COLUMN IF NOT EXISTS account_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS account_priced_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS account_unpriced_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS account_cost_floor_applied_requests BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS account_raw_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS account_shaped_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS account_uplifted_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS account_profit_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS account_reported_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS account_billable_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS account_cost_floor_delta_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS duration_ms_sum BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS duration_ms_count BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS duration_ms_max BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS account_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS account_raw_tokens BIGINT NOT NULL DEFAULT 0,
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

ALTER TABLE model_pricing
    ADD COLUMN IF NOT EXISTS cache_creation_1h_input_token_cost DOUBLE PRECISION NOT NULL DEFAULT 0;

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
    listed BOOLEAN NOT NULL DEFAULT true,
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





    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);







CREATE TABLE IF NOT EXISTS account_quality_samples (
    account_id BIGINT NOT NULL,
    sampled_at TIMESTAMPTZ NOT NULL,
    recent_error_rate DOUBLE PRECISION NOT NULL DEFAULT 0,
    ttft_ewma_ms DOUBLE PRECISION,
    latency_ewma_ms DOUBLE PRECISION,
    sample_count BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (account_id, sampled_at)
);

CREATE INDEX IF NOT EXISTS idx_account_quality_samples_sampled_at
    ON account_quality_samples (sampled_at);

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

CREATE TABLE IF NOT EXISTS route_scenarios (
    id BIGSERIAL PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    content JSONB NOT NULL DEFAULT '{}'::jsonb,
    revision BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
"#;

// Explicit maintenance-only backfill. Do not call this from the default startup
// migration path; it can scan/update historical usage tables on large installs.
const USAGE_LEGACY_COST_FIELD_BACKFILL_SQL: &str = r#"
UPDATE usage_records
SET original_cost_usd = COALESCE(
    NULLIF(data #>> '{accountBilling,rawCostUsd}', '')::double precision,
    NULLIF(data #>> '{originalCostUsd}', '')::double precision,
    estimated_cost_usd,
    0
)
WHERE original_cost_usd = 0
  AND (
      estimated_cost_usd <> 0
      OR NULLIF(data #>> '{accountBilling,rawCostUsd}', '') IS NOT NULL
      OR NULLIF(data #>> '{originalCostUsd}', '') IS NOT NULL
  );

UPDATE usage_rollup_totals
SET total_original_cost_usd = CASE
    WHEN dimension = 'account' AND account_raw_cost_usd <> 0
    THEN account_raw_cost_usd
    ELSE total_estimated_cost_usd
END
WHERE total_original_cost_usd = 0
  AND (total_estimated_cost_usd <> 0 OR account_raw_cost_usd <> 0);

UPDATE usage_rollup_time_buckets
SET total_original_cost_usd = CASE
    WHEN dimension = 'account' AND account_raw_cost_usd <> 0
    THEN account_raw_cost_usd
    ELSE total_estimated_cost_usd
END
WHERE total_original_cost_usd = 0
  AND (total_estimated_cost_usd <> 0 OR account_raw_cost_usd <> 0);

UPDATE usage_rollup_totals
SET account_shaped_cost_usd = account_reported_cost_usd,
    account_uplifted_cost_usd = account_reported_cost_usd,
    account_profit_usd = account_reported_cost_usd - account_raw_cost_usd
WHERE account_requests > 0
  AND account_reported_cost_usd <> 0
  AND account_shaped_cost_usd = 0
  AND account_uplifted_cost_usd = 0
  AND account_profit_usd = 0;

UPDATE usage_rollup_time_buckets
SET account_shaped_cost_usd = account_reported_cost_usd,
    account_uplifted_cost_usd = account_reported_cost_usd,
    account_profit_usd = account_reported_cost_usd - account_raw_cost_usd
WHERE account_requests > 0
  AND account_reported_cost_usd <> 0
  AND account_shaped_cost_usd = 0
  AND account_uplifted_cost_usd = 0
  AND account_profit_usd = 0;
"#;

const ACCOUNT_DEFAULT_MODEL_MAPPING_PASSTHROUGH_SQL: &str = r#"
ALTER TABLE accounts
    ALTER COLUMN model_mapping_mode SET DEFAULT 'passthrough_mapping';

UPDATE accounts
SET model_mapping_mode = 'passthrough_mapping',
    revision = revision + 1,
    updated_at = now()
WHERE deleted_at IS NULL
  AND model_mapping_mode = 'processed_mapping'
  AND normalize_model_version_dots = false
  AND model_mapping_require_match = false
  AND model_mapping_rules = '[]'::jsonb;
"#;

const USAGE_CLEANUP_BATCH_SIZE_LIMIT_SQL: &str = r#"
ALTER TABLE usage_cleanup_jobs
    DROP CONSTRAINT IF EXISTS usage_cleanup_jobs_batch_size_check;

ALTER TABLE usage_cleanup_jobs
    ADD CONSTRAINT usage_cleanup_jobs_batch_size_check
    CHECK (batch_size > 0 AND batch_size <= 5000);
"#;

const USAGE_CLEANUP_BATCH_SIZE_LIMIT_V2_SQL: &str = r#"
ALTER TABLE usage_cleanup_jobs
    DROP CONSTRAINT IF EXISTS usage_cleanup_jobs_batch_size_check;

ALTER TABLE usage_cleanup_jobs
    ADD CONSTRAINT usage_cleanup_jobs_batch_size_check
    CHECK (batch_size > 0 AND batch_size <= 10000);
"#;

const USAGE_CLEANUP_SEMANTICS_SQL: &str = r#"
ALTER TABLE usage_cleanup_jobs
    ADD COLUMN IF NOT EXISTS include_summary BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS max_rows BIGINT;

ALTER TABLE usage_cleanup_jobs
    DROP CONSTRAINT IF EXISTS usage_cleanup_jobs_max_rows_check;

ALTER TABLE usage_cleanup_jobs
    ADD CONSTRAINT usage_cleanup_jobs_max_rows_check
    CHECK (max_rows IS NULL OR max_rows >= 0);
"#;

/// Frozen: databases that already ran v1 hold this checksum. Kept only so the
/// v2 test can prove the two differ solely by `total_compat_input_tokens`.
#[cfg(test)]
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

    COALESCE(SUM(account_requests), 0)::bigint AS account_requests,
    COALESCE(SUM(account_priced_requests), 0)::bigint AS account_priced_requests,
    COALESCE(SUM(account_unpriced_requests), 0)::bigint AS account_unpriced_requests,
    COALESCE(SUM(account_cost_floor_applied_requests), 0)::bigint AS account_cost_floor_applied_requests,
    COALESCE(SUM(account_raw_cost_usd), 0)::double precision AS account_raw_cost_usd,
    COALESCE(SUM(account_shaped_cost_usd), 0)::double precision AS account_shaped_cost_usd,
    COALESCE(SUM(account_uplifted_cost_usd), 0)::double precision AS account_uplifted_cost_usd,
    COALESCE(SUM(account_profit_usd), 0)::double precision AS account_profit_usd,
    COALESCE(SUM(account_reported_cost_usd), 0)::double precision AS account_reported_cost_usd,
    COALESCE(SUM(account_billable_cost_usd), 0)::double precision AS account_billable_cost_usd,
    COALESCE(SUM(account_cost_floor_delta_usd), 0)::double precision AS account_cost_floor_delta_usd,
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
    simulated_requests, upstream_metadata_requests, total_input_tokens, billable_input_tokens,
    total_output_tokens, total_cache_read_input_tokens,
    total_cache_creation_input_tokens, local_prompt_cache_input_tokens,
    local_prompt_cache_read_input_tokens, local_prompt_cache_creation_input_tokens,
    total_estimated_cost_usd, total_original_cost_usd,
    account_requests, account_priced_requests, account_unpriced_requests,
    account_cost_floor_applied_requests, account_raw_cost_usd,
    account_shaped_cost_usd, account_uplifted_cost_usd,
    account_profit_usd, account_reported_cost_usd,
    account_billable_cost_usd, account_cost_floor_delta_usd,
    duration_ms_sum, duration_ms_count, duration_ms_max, updated_at
)
SELECT
    bucket_start, dimension, dimension_key, dimension_label, requests,
    success_requests, error_requests, stream_requests, non_stream_requests,
    priced_requests, unpriced_requests, local_prompt_cache_requests,
    simulated_requests, upstream_metadata_requests, total_input_tokens, billable_input_tokens,
    total_output_tokens, total_cache_read_input_tokens,
    total_cache_creation_input_tokens, local_prompt_cache_input_tokens,
    local_prompt_cache_read_input_tokens, local_prompt_cache_creation_input_tokens,
    total_estimated_cost_usd, total_original_cost_usd, account_requests, account_priced_requests, account_unpriced_requests,
    account_cost_floor_applied_requests, account_raw_cost_usd,
    account_shaped_cost_usd, account_uplifted_cost_usd,
    account_profit_usd, account_reported_cost_usd,
    account_billable_cost_usd, account_cost_floor_delta_usd,
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

/// v1 (above) predates `total_compat_input_tokens`; its checksum is frozen in
/// databases that already ran it, so the column-aware copy lives in v2. A
/// database runs at most one of them: v2 is skipped once v1 has compressed.
#[cfg(test)]
const USAGE_ROLLUP_HOUR_BUCKET_COMPRESSION_V2_SQL: &str = r#"
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


    COALESCE(SUM(total_input_tokens), 0)::bigint AS total_input_tokens,
    COALESCE(SUM(billable_input_tokens), 0)::bigint AS billable_input_tokens,
    COALESCE(SUM(total_compat_input_tokens), 0)::bigint AS total_compat_input_tokens,
    COALESCE(SUM(total_output_tokens), 0)::bigint AS total_output_tokens,
    COALESCE(SUM(total_cache_read_input_tokens), 0)::bigint AS total_cache_read_input_tokens,
    COALESCE(SUM(total_cache_creation_input_tokens), 0)::bigint AS total_cache_creation_input_tokens,
    COALESCE(SUM(local_prompt_cache_input_tokens), 0)::bigint AS local_prompt_cache_input_tokens,
    COALESCE(SUM(local_prompt_cache_read_input_tokens), 0)::bigint AS local_prompt_cache_read_input_tokens,
    COALESCE(SUM(local_prompt_cache_creation_input_tokens), 0)::bigint AS local_prompt_cache_creation_input_tokens,
    COALESCE(SUM(total_estimated_cost_usd), 0)::double precision AS total_estimated_cost_usd,
    COALESCE(SUM(total_original_cost_usd), 0)::double precision AS total_original_cost_usd,

    COALESCE(SUM(account_requests), 0)::bigint AS account_requests,
    COALESCE(SUM(account_priced_requests), 0)::bigint AS account_priced_requests,
    COALESCE(SUM(account_unpriced_requests), 0)::bigint AS account_unpriced_requests,
    COALESCE(SUM(account_cost_floor_applied_requests), 0)::bigint AS account_cost_floor_applied_requests,
    COALESCE(SUM(account_raw_cost_usd), 0)::double precision AS account_raw_cost_usd,
    COALESCE(SUM(account_shaped_cost_usd), 0)::double precision AS account_shaped_cost_usd,
    COALESCE(SUM(account_uplifted_cost_usd), 0)::double precision AS account_uplifted_cost_usd,
    COALESCE(SUM(account_profit_usd), 0)::double precision AS account_profit_usd,
    COALESCE(SUM(account_reported_cost_usd), 0)::double precision AS account_reported_cost_usd,
    COALESCE(SUM(account_billable_cost_usd), 0)::double precision AS account_billable_cost_usd,
    COALESCE(SUM(account_cost_floor_delta_usd), 0)::double precision AS account_cost_floor_delta_usd,
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
    simulated_requests, upstream_metadata_requests, total_input_tokens, billable_input_tokens,
    total_compat_input_tokens,
    total_output_tokens, total_cache_read_input_tokens,
    total_cache_creation_input_tokens, local_prompt_cache_input_tokens,
    local_prompt_cache_read_input_tokens, local_prompt_cache_creation_input_tokens,
    total_estimated_cost_usd, total_original_cost_usd,
    account_requests, account_priced_requests, account_unpriced_requests,
    account_cost_floor_applied_requests, account_raw_cost_usd,
    account_shaped_cost_usd, account_uplifted_cost_usd,
    account_profit_usd, account_reported_cost_usd,
    account_billable_cost_usd, account_cost_floor_delta_usd,
    duration_ms_sum, duration_ms_count, duration_ms_max, updated_at
)
SELECT
    bucket_start, dimension, dimension_key, dimension_label, requests,
    success_requests, error_requests, stream_requests, non_stream_requests,
    priced_requests, unpriced_requests, local_prompt_cache_requests,
    simulated_requests, upstream_metadata_requests, total_input_tokens, billable_input_tokens,
    total_compat_input_tokens,
    total_output_tokens, total_cache_read_input_tokens,
    total_cache_creation_input_tokens, local_prompt_cache_input_tokens,
    local_prompt_cache_read_input_tokens, local_prompt_cache_creation_input_tokens,
    total_estimated_cost_usd, total_original_cost_usd, account_requests, account_priced_requests, account_unpriced_requests,
    account_cost_floor_applied_requests, account_raw_cost_usd,
    account_shaped_cost_usd, account_uplifted_cost_usd,
    account_profit_usd, account_reported_cost_usd,
    account_billable_cost_usd, account_cost_floor_delta_usd,
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

/// v3：v2 漏了账号成本与原始 token 两列，压缩后会按默认值写成 0。新库一律跑 v3；
/// v1/v2 已执行过的库不再重复压缩。
const USAGE_ROLLUP_HOUR_BUCKET_COMPRESSION_V3_SQL: &str = r#"
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


    COALESCE(SUM(total_input_tokens), 0)::bigint AS total_input_tokens,
    COALESCE(SUM(billable_input_tokens), 0)::bigint AS billable_input_tokens,
    COALESCE(SUM(total_compat_input_tokens), 0)::bigint AS total_compat_input_tokens,
    COALESCE(SUM(total_output_tokens), 0)::bigint AS total_output_tokens,
    COALESCE(SUM(total_cache_read_input_tokens), 0)::bigint AS total_cache_read_input_tokens,
    COALESCE(SUM(total_cache_creation_input_tokens), 0)::bigint AS total_cache_creation_input_tokens,
    COALESCE(SUM(local_prompt_cache_input_tokens), 0)::bigint AS local_prompt_cache_input_tokens,
    COALESCE(SUM(local_prompt_cache_read_input_tokens), 0)::bigint AS local_prompt_cache_read_input_tokens,
    COALESCE(SUM(local_prompt_cache_creation_input_tokens), 0)::bigint AS local_prompt_cache_creation_input_tokens,
    COALESCE(SUM(total_estimated_cost_usd), 0)::double precision AS total_estimated_cost_usd,
    COALESCE(SUM(total_original_cost_usd), 0)::double precision AS total_original_cost_usd,

    COALESCE(SUM(account_requests), 0)::bigint AS account_requests,
    COALESCE(SUM(account_priced_requests), 0)::bigint AS account_priced_requests,
    COALESCE(SUM(account_unpriced_requests), 0)::bigint AS account_unpriced_requests,
    COALESCE(SUM(account_cost_floor_applied_requests), 0)::bigint AS account_cost_floor_applied_requests,
    COALESCE(SUM(account_raw_cost_usd), 0)::double precision AS account_raw_cost_usd,
    COALESCE(SUM(account_shaped_cost_usd), 0)::double precision AS account_shaped_cost_usd,
    COALESCE(SUM(account_uplifted_cost_usd), 0)::double precision AS account_uplifted_cost_usd,
    COALESCE(SUM(account_profit_usd), 0)::double precision AS account_profit_usd,
    COALESCE(SUM(account_reported_cost_usd), 0)::double precision AS account_reported_cost_usd,
    COALESCE(SUM(account_billable_cost_usd), 0)::double precision AS account_billable_cost_usd,
    COALESCE(SUM(account_cost_floor_delta_usd), 0)::double precision AS account_cost_floor_delta_usd,
    COALESCE(SUM(account_cost_usd), 0)::double precision AS account_cost_usd,
    COALESCE(SUM(account_raw_tokens), 0)::bigint AS account_raw_tokens,
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
    simulated_requests, upstream_metadata_requests, total_input_tokens, billable_input_tokens,
    total_compat_input_tokens,
    total_output_tokens, total_cache_read_input_tokens,
    total_cache_creation_input_tokens, local_prompt_cache_input_tokens,
    local_prompt_cache_read_input_tokens, local_prompt_cache_creation_input_tokens,
    total_estimated_cost_usd, total_original_cost_usd,
    account_requests, account_priced_requests, account_unpriced_requests,
    account_cost_floor_applied_requests, account_raw_cost_usd,
    account_shaped_cost_usd, account_uplifted_cost_usd,
    account_profit_usd, account_reported_cost_usd,
    account_billable_cost_usd, account_cost_floor_delta_usd,
    account_cost_usd, account_raw_tokens,
    duration_ms_sum, duration_ms_count, duration_ms_max, updated_at
)
SELECT
    bucket_start, dimension, dimension_key, dimension_label, requests,
    success_requests, error_requests, stream_requests, non_stream_requests,
    priced_requests, unpriced_requests, local_prompt_cache_requests,
    simulated_requests, upstream_metadata_requests, total_input_tokens, billable_input_tokens,
    total_compat_input_tokens,
    total_output_tokens, total_cache_read_input_tokens,
    total_cache_creation_input_tokens, local_prompt_cache_input_tokens,
    local_prompt_cache_read_input_tokens, local_prompt_cache_creation_input_tokens,
    total_estimated_cost_usd, total_original_cost_usd, account_requests, account_priced_requests, account_unpriced_requests,
    account_cost_floor_applied_requests, account_raw_cost_usd,
    account_shaped_cost_usd, account_uplifted_cost_usd,
    account_profit_usd, account_reported_cost_usd,
    account_billable_cost_usd, account_cost_floor_delta_usd,
    account_cost_usd, account_raw_tokens,
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

fn remap_configuration_account_ids(
    config: &mut Config,
    scenarios: &mut [serde_json::Value],
    ids: &std::collections::BTreeMap<u64, u64>,
) -> anyhow::Result<()> {
    fn remap_routes(
        routes: &mut crate::model::routes::RoutesConfig,
        ids: &std::collections::BTreeMap<u64, u64>,
    ) -> anyhow::Result<()> {
        for route in &mut routes.routes {
            for rule in &mut route.account_rule.rules {
                if rule == "*" {
                    continue;
                }
                let id = rule.parse::<u64>()?;
                *rule = ids
                    .get(&id)
                    .ok_or_else(|| anyhow::anyhow!("路由引用了导入包中不存在的账号: {id}"))?
                    .to_string();
            }
        }
        Ok(())
    }
    remap_routes(&mut config.routes, ids)?;
    for value in scenarios {
        let mut routes: crate::model::routes::RoutesConfig =
            serde_json::from_value(value["content"]["routes"].clone())?;
        remap_routes(&mut routes, ids)?;
        value["content"]["routes"] = serde_json::to_value(routes)?;
        if let Some(accounts) = value["content"]["externalAccounts"].as_array_mut() {
            for account in accounts {
                let id = account["id"]
                    .as_u64()
                    .ok_or_else(|| anyhow::anyhow!("场景账号缺少 id"))?;
                account["id"] = serde_json::json!(
                    ids.get(&id)
                        .ok_or_else(|| anyhow::anyhow!("场景引用了导入包中不存在的账号: {id}"))?
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
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
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    use super::*;
    use crate::anthropic::usage::{
        AccountAttempt, AccountBilling, AccountUsageSnapshot, UsageRecordStatus, UsageRouteKind,
        UsageRouteSubtype, UsageSource,
    };
    use serde_json::json;

    #[tokio::test]
    async fn production_pool_discards_connections_left_inside_a_transaction() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
            return;
        };
        let url = config.postgres.url.clone().unwrap();
        let pool = production_pool_options(1).connect(&url).await.unwrap();

        let mut conn = pool.acquire().await.unwrap();
        let timeout: String = sqlx::query_scalar("SHOW idle_in_transaction_session_timeout")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert_eq!(timeout, "1min");
        let first_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        // What a cancelled `begin()` leaves behind: BEGIN ran, sqlx never counted it.
        sqlx::query("BEGIN").execute(&mut *conn).await.unwrap();
        drop(conn);

        let mut conn = pool.acquire().await.unwrap();
        let second_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert_ne!(
            first_pid, second_pid,
            "the dangling connection must not be reused"
        );
        assert!(outside_transaction(&mut conn).await.unwrap());
        drop(conn);

        // A clean connection is kept.
        let mut conn = pool.acquire().await.unwrap();
        let third_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert_eq!(second_pid, third_pid);
        drop(conn);
        pool.close().await;
    }

    fn test_config() -> Option<Config> {
        let url = crate::storage::integration_test_url("CLAUDE_POOL_TEST_POSTGRES_URL")?;
        let mut config = Config::default();
        config.postgres.url = Some(url);
        config.postgres.max_connections = 2;
        Some(config)
    }

    #[test]
    fn new_account_model_and_usage_columns_are_in_create_table_definitions() {
        for (table, columns) in [
            (
                "accounts",
                vec![
                    "tags TEXT[]",
                    "proxy_url TEXT",
                    "cost_multiplier DOUBLE PRECISION",
                    "overrides JSONB",
                ],
            ),
            (
                "model_capabilities",
                vec!["listed BOOLEAN NOT NULL DEFAULT true"],
            ),
            (
                "usage_records",
                vec!["account_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0"],
            ),
            (
                "usage_rollup_totals",
                vec![
                    "account_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0",
                    "account_raw_tokens BIGINT NOT NULL DEFAULT 0",
                ],
            ),
            (
                "usage_rollup_time_buckets",
                vec![
                    "account_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0",
                    "account_raw_tokens BIGINT NOT NULL DEFAULT 0",
                ],
            ),
        ] {
            let create = SCHEMA_SQL
                .split(&format!("CREATE TABLE IF NOT EXISTS {table} ("))
                .nth(1)
                .unwrap()
                .split("\n);")
                .next()
                .unwrap();
            for column in columns {
                assert!(create.contains(column), "{table}: {column}");
            }
        }
    }

    #[test]
    fn required_postgres_schema_columns_cover_known_upgrade_breakers() {
        let required: HashSet<(&str, &str)> = REQUIRED_POSTGRES_SCHEMA_COLUMNS
            .iter()
            .map(|column| (column.table_name, column.column_name))
            .collect();
        for pair in [
            ("accounts", "revision"),
            ("accounts", "pre_output_stream_retry_mode"),
            ("accounts", "route_mode"),
            ("accounts", "route_rules"),
            ("usage_records", "rollup_active"),
            ("usage_cleanup_jobs", "batch_size"),
            ("usage_records", "original_cost_usd"),
            ("account_quality_samples", "sample_count"),
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
                !(required.table_name == "accounts" && required.column_name == "revision")
            })
            .map(|required| {
                (
                    required.table_name.to_string(),
                    required.column_name.to_string(),
                )
            })
            .collect();

        let missing = required_postgres_schema_missing_columns(&present);
        assert_eq!(missing, vec!["accounts.revision"]);
    }

    #[tokio::test]
    async fn account_change_log_filters_summarizes_and_expires() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
            return;
        };
        let store = PostgresStore::connect_test(&config).await.unwrap();
        let event = |kind: &str, source: &str, label: &str| json!({ "accountEvent": kind, "accountSource": source, "accountLabel": label });
        for (actor, action, object_type, object_id, detail) in [
            (
                "system-scheduler",
                "auto_disable_credential",
                "credential",
                "1",
                event("quota_exhausted", "local", "a@example.com"),
            ),
            (
                "system-scheduler",
                "auto_disable_credential",
                "credential",
                "2",
                event("quota_exhausted", "local", "b@example.com"),
            ),
            (
                "system-scheduler",
                "auto_disable_credential",
                "credential",
                "2",
                event("auto_disabled", "local", "b@example.com"),
            ),
            (
                "system-account",
                "account_account_event",
                "account",
                "9",
                event("quota_exhausted", "external", "pool-nine"),
            ),
            (
                "admin-api",
                "update_runtime_config",
                "runtime_config",
                "",
                json!({}),
            ),
        ] {
            store
                .record_admin_audit_log(
                    actor,
                    action,
                    object_type,
                    Some(object_id).filter(|id| !id.is_empty()),
                    true,
                    None,
                    detail,
                )
                .await
                .unwrap();
        }
        // One old account event that retention should remove.
        sqlx::query(
            "INSERT INTO admin_audit_logs (actor, action, object_type, object_id, success, detail, created_at) VALUES ('system-scheduler', 'auto_disable_credential', 'credential', '3', true, $1, now() - interval '20 days')",
        )
        .bind(event("auto_disabled", "local", "old@example.com"))
        .execute(store.pool())
        .await
        .unwrap();

        let page = |filter: AdminAuditLogFilter| {
            let store = &store;
            async move {
                store
                    .query_admin_audit_logs_filtered(1, 50, &filter)
                    .await
                    .unwrap()
            }
        };
        let all = page(AdminAuditLogFilter::default()).await;
        assert_eq!(all.records.len(), 6);
        let events = page(AdminAuditLogFilter {
            account_events_only: true,
            ..Default::default()
        })
        .await;
        assert_eq!(events.records.len(), 5);
        let quota = page(AdminAuditLogFilter {
            account_event: Some("quota_exhausted".into()),
            ..Default::default()
        })
        .await;
        assert_eq!(quota.records.len(), 3);
        let local_quota = page(AdminAuditLogFilter {
            account_event: Some("quota_exhausted".into()),
            account_source: Some("local".into()),
            ..Default::default()
        })
        .await;
        assert_eq!(local_quota.records.len(), 2);
        let by_label = page(AdminAuditLogFilter {
            account_query: Some("B@EXAMPLE".into()),
            ..Default::default()
        })
        .await;
        assert_eq!(by_label.records.len(), 2);
        let by_id = page(AdminAuditLogFilter {
            account_query: Some("#9".into()),
            ..Default::default()
        })
        .await;
        assert_eq!(by_id.records.len(), 1);
        // LIKE wildcards in the search are literal.
        let wildcard = page(AdminAuditLogFilter {
            account_query: Some("%".into()),
            ..Default::default()
        })
        .await;
        assert!(wildcard.records.is_empty());
        let recent = page(AdminAuditLogFilter {
            account_events_only: true,
            since: Some(Utc::now() - chrono::Duration::days(1)),
            ..Default::default()
        })
        .await;
        assert_eq!(recent.records.len(), 4);
        // 分类 = 动作列表；全文搜索覆盖 action / actor / object / 错误与账号标签，服务端执行。
        let by_actions = page(AdminAuditLogFilter {
            actions: Some(vec![
                "auto_disable_credential".into(),
                "missing_action".into(),
            ]),
            ..Default::default()
        })
        .await;
        assert!(!by_actions.records.is_empty());
        assert!(
            by_actions
                .records
                .iter()
                .all(|row| row.action == "auto_disable_credential")
        );
        let empty_actions = page(AdminAuditLogFilter {
            actions: Some(vec![" ".into()]),
            ..Default::default()
        })
        .await;
        assert_eq!(
            empty_actions.records.len(),
            6,
            "blank action list means no filter"
        );
        let by_text = page(AdminAuditLogFilter {
            text_query: Some("OLD@EXAMPLE".into()),
            ..Default::default()
        })
        .await;
        assert_eq!(
            by_text.records.len(),
            1,
            "account label inside detail is searchable"
        );
        let by_actor = page(AdminAuditLogFilter {
            text_query: Some("scheduler".into()),
            ..Default::default()
        })
        .await;
        assert!(
            by_actor
                .records
                .iter()
                .all(|row| row.actor.contains("scheduler"))
        );
        assert!(!by_actor.records.is_empty());
        let like_escaped = page(AdminAuditLogFilter {
            text_query: Some("%".into()),
            ..Default::default()
        })
        .await;
        assert!(like_escaped.records.is_empty(), "wildcards are literal");
        // Paging stays exact under a filter.
        let first = store
            .query_admin_audit_logs_filtered(
                1,
                2,
                &AdminAuditLogFilter {
                    account_events_only: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(first.records.len(), 2);
        assert!(first.has_next);

        let mut summary = store
            .summarize_account_events(Utc::now() - chrono::Duration::days(1), None, None, None)
            .await
            .unwrap();
        summary.sort_by(|a, b| a.event.cmp(&b.event));
        let summary: Vec<_> = summary
            .iter()
            .map(|row| (row.event.as_str(), row.events, row.accounts))
            .collect();
        assert_eq!(
            summary,
            vec![("auto_disabled", 1, 1), ("quota_exhausted", 3, 3)]
        );
        // 按账号过滤：只统计 #9 的异动。
        let only_nine = store
            .summarize_account_events(Utc::now() - chrono::Duration::days(1), None, None, Some(9))
            .await
            .unwrap();
        assert_eq!(
            only_nine
                .iter()
                .map(|row| (row.event.as_str(), row.events))
                .collect::<Vec<_>>(),
            vec![("quota_exhausted", 1)]
        );
        // 窗口终点早于全部事件时为空（与查询窗口对齐，而不是从现在往前数）。
        let before = store
            .summarize_account_events(
                Utc::now() - chrono::Duration::days(1),
                Some(Utc::now() - chrono::Duration::hours(1)),
                None,
                None,
            )
            .await
            .unwrap();
        assert!(before.is_empty(), "{before:?}");
        let local = store
            .summarize_account_events(
                Utc::now() - chrono::Duration::days(1),
                None,
                Some("local"),
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            local
                .iter()
                .find(|row| row.event == "quota_exhausted")
                .unwrap()
                .events,
            2
        );

        let retention = std::time::Duration::from_secs(15 * 24 * 60 * 60);
        assert_eq!(
            store
                .delete_expired_admin_audit_logs(retention, 100)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .delete_expired_admin_audit_logs(retention, 100)
                .await
                .unwrap(),
            0
        );
        assert_eq!(page(AdminAuditLogFilter::default()).await.records.len(), 5);

        store.drop_test_schema().await.unwrap();
    }

    async fn clean(store: &PostgresStore) {
        for statement in [
            "TRUNCATE TABLE admin_audit_logs",
            "TRUNCATE TABLE account_quality_samples",
            "TRUNCATE TABLE usage_cleanup_jobs",
            "TRUNCATE TABLE usage_cleanup_watermarks",
            "TRUNCATE TABLE usage_duration_rollup_time_buckets",
            "TRUNCATE TABLE usage_cache_read_rollup_time_buckets",
            "TRUNCATE TABLE usage_cache_read_totals",
            "TRUNCATE TABLE usage_rollup_time_buckets",
            "TRUNCATE TABLE usage_rollup_totals",
            "TRUNCATE TABLE usage_records",
            "TRUNCATE TABLE model_pricing_sync_status",
            "TRUNCATE TABLE model_pricing",
            "TRUNCATE TABLE accounts CASCADE",
            "TRUNCATE TABLE runtime_config",
        ] {
            sqlx::query(statement).execute(store.pool()).await.unwrap();
        }
    }

    #[tokio::test]
    async fn postgres_schema_compatibility_check_rejects_missing_upgrade_column() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
            return;
        };
        let store = PostgresStore::connect_test(&config).await.unwrap();

        sqlx::query("ALTER TABLE accounts DROP COLUMN revision")
            .execute(store.pool())
            .await
            .unwrap();

        let error = store
            .verify_required_schema_compatibility(false)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("accounts.revision"));
        assert!(error.contains("CLAUDE_POOL_POSTGRES_MIGRATE_ON_START=true"));
        assert!(error.contains("startup migration is disabled"));

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_usage_pool_isolated_from_exhausted_main_pool_for_three_rounds() {
        let Some(mut config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
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
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
            return;
        };
        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;

        for statement in [
            "ALTER TABLE usage_records DROP COLUMN pricing_available",
            "ALTER TABLE usage_records DROP COLUMN duration_ms",
            "ALTER TABLE usage_records DROP COLUMN data",
            "ALTER TABLE usage_rollup_totals DROP COLUMN account_reported_cost_usd",
            "ALTER TABLE usage_rollup_totals DROP COLUMN duration_ms_sum",
            "ALTER TABLE usage_rollup_time_buckets DROP COLUMN account_reported_cost_usd",
            "ALTER TABLE usage_rollup_time_buckets DROP COLUMN duration_ms_sum",
        ] {
            sqlx::query(statement).execute(store.pool()).await.unwrap();
        }

        let missing_before = store
            .verify_required_schema_compatibility(false)
            .await
            .unwrap_err()
            .to_string();
        assert!(missing_before.contains("usage_records.pricing_available"));
        assert!(missing_before.contains("usage_rollup_totals.account_reported_cost_usd"));
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
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
            return;
        };
        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;

        sqlx::query(
            "DELETE FROM schema_migrations WHERE version IN ('usage-cleanup-batch-size-limit-v1', 'usage-cleanup-batch-size-limit-v2')",
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
        assert!(compatibility_error.contains("usage_cleanup_jobs.batch_size_check<=10000"));

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
                    include_summary: false,
                    cutoff_at: Utc::now(),
                    batch_size: 10_000,
                    max_batches: 100,
                    max_rows: None,
                    pause_ms_between_batches: 0,
                })
                .await
                .unwrap(),
            "expanded cleanup batch-size constraint must accept 10000"
        );

        store.drop_test_schema().await.unwrap();
    }

    fn usage_record(id: &str, cache_read: i32) -> UsageRecord {
        UsageRecord {
            account_cost_usd: 0.0,
            id: id.to_string(),
            created_at: Utc::now().to_rfc3339(),
            endpoint: "/v1/messages".to_string(),
            stream: false,
            model: "claude-sonnet-4-5".to_string(),
            requested_max_tokens: None,
            downstream_stop_reason: None,
            upstream_model: None,
            external_outbound_model: None,
            upstream_response_model: None,
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

            pricing_available: true,
            pricing_model: Some("claude-sonnet-4-5".to_string()),
            duration_ms: 30,
            first_token_latency_ms: None,
            response_latency_ms: Some(30),
            latency_trace: None,
            simulated: true,

            route_kind: None,
            route_subtype: None,

            account_id: None,
            account_name: None,
            external_attempts: Vec::new(),
            usage_projection_applied: None,
            account_billing: None,

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

    #[test]
    fn usage_rollup_metrics_track_uncached_input_in_both_directions() {
        let mut record = usage_record("compat-input", 300);
        record.compat_input_tokens = 120;
        let added = UsageRollupMetrics::from_record(&record, 1);
        let removed = UsageRollupMetrics::from_record(&record, -1);
        assert_eq!(added.total_compat_input_tokens, 120);
        assert_eq!(removed.total_compat_input_tokens, -120);
        let mut net = added;
        net.add(removed);
        assert_eq!(net.total_compat_input_tokens, 0);
        assert_eq!(net.total_cache_read_input_tokens, 0);
    }

    /// 压缩会 TRUNCATE 时间桶表再插回去：插回的列必须覆盖表上的每一列，
    /// 否则漏掉的列会按默认值写成 0（v2 漏了账号成本与原始 token）。
    #[test]
    fn usage_rollup_hour_compression_v3_carries_every_time_bucket_column() {
        let table_start = SCHEMA_SQL
            .find("CREATE TABLE IF NOT EXISTS usage_rollup_time_buckets (")
            .expect("time bucket table");
        let table_end = SCHEMA_SQL[table_start..]
            .find(");")
            .map(|offset| table_start + offset)
            .unwrap();
        let columns = SCHEMA_SQL[table_start..table_end]
            .lines()
            .skip(1)
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with("PRIMARY KEY"))
            .filter_map(|line| line.split_whitespace().next())
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert!(
            columns.contains(&"account_cost_usd".to_string()),
            "{columns:?}"
        );
        let insert_start = USAGE_ROLLUP_HOUR_BUCKET_COMPRESSION_V3_SQL
            .find("INSERT INTO usage_rollup_time_buckets (")
            .unwrap();
        let insert_end = USAGE_ROLLUP_HOUR_BUCKET_COMPRESSION_V3_SQL[insert_start..]
            .find(")\nSELECT")
            .map(|offset| insert_start + offset)
            .unwrap();
        let inserted = USAGE_ROLLUP_HOUR_BUCKET_COMPRESSION_V3_SQL[insert_start..insert_end]
            .split(['(', ',', '\n'])
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .collect::<Vec<_>>();
        for column in &columns {
            assert!(
                inserted.contains(&column.as_str()),
                "compression v3 drops column {column}"
            );
            assert!(
                USAGE_ROLLUP_HOUR_BUCKET_COMPRESSION_V3_SQL.contains(&format!("AS {column},"))
                    || USAGE_ROLLUP_HOUR_BUCKET_COMPRESSION_V3_SQL
                        .contains(&format!("AS {column}\n"))
                    || [
                        "bucket_start",
                        "dimension",
                        "dimension_key",
                        "dimension_label"
                    ]
                    .contains(&column.as_str()),
                "compression v3 does not aggregate column {column}"
            );
        }
    }

    #[test]
    fn usage_rollup_hour_compression_v2_only_adds_the_compat_input_column() {
        let v1 = USAGE_ROLLUP_HOUR_BUCKET_COMPRESSION_SQL;
        let v2 = USAGE_ROLLUP_HOUR_BUCKET_COMPRESSION_V2_SQL;
        assert!(!v1.contains("total_compat_input_tokens"));
        assert_eq!(
            v2.matches("total_compat_input_tokens").count(),
            4,
            "v2 must sum the column and carry it through the temp-table insert"
        );
        let stripped = v2
            .replace(
                "    COALESCE(SUM(total_compat_input_tokens), 0)::bigint AS total_compat_input_tokens,\n",
                "",
            )
            .replace("    total_compat_input_tokens,\n", "");
        assert_eq!(
            stripped, v1,
            "v2 must not drift from v1 beyond the new column"
        );
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
        });

        assert_eq!(window.key, "today");
        assert_eq!(window.summary.total_requests, 100);
        assert_eq!(window.summary.success_requests, 97);
        assert_eq!(window.summary.error_requests, 3);
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
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
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
                    include_summary: false,
                    cutoff_at: cutoff,
                    batch_size: 250,
                    max_batches: 100,
                    max_rows: None,
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
                    include_summary: false,
                    cutoff_at: cutoff,
                    batch_size: 250,
                    max_batches: 100,
                    max_rows: None,
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
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
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
                    include_summary: false,
                    cutoff_at: Utc::now() - chrono::Duration::days(7),
                    batch_size: 250,
                    max_batches: 100,
                    max_rows: None,
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
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
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
                    include_summary: false,
                    cutoff_at: Utc::now(),
                    batch_size: 10,
                    max_batches: 10,
                    max_rows: None,
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
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
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
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
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
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
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
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
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
                       account_raw_cost_usd, duration_ms_max
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
    async fn postgres_account_usage_risk_reports_cache_and_cost_risks() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
            return;
        };
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;
        let usage_store = PostgresUsageStore::new(store.clone());

        let mut high_cache = external_usage_record("external-risk-high-cache", 0.20, 0.18, 0.19);
        high_cache.account_billing.as_mut().unwrap().raw_usage =
            external_usage_snapshot(1_000, 20, 910_000, 120_000);
        high_cache.raw_usage = high_cache
            .account_billing
            .as_ref()
            .map(|billing| billing.raw_usage);
        usage_store.record(high_cache).await.unwrap();

        let ok = external_usage_record("external-risk-ok", 0.10, 0.11, 0.12);
        usage_store.record(ok).await.unwrap();

        let response = usage_store
            .account_usage_risk(
                UsageAccountRiskQuery {
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
                UsageAccountRiskCostConfig {
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
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
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

    /// 总览的账号成本与原始 token 直接来自汇总表；写入、负向抵消、回填三条路径口径一致。
    #[tokio::test]
    async fn postgres_overview_totals_come_from_rollups_and_backfill_matches() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
            return;
        };
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;
        let usage_store = PostgresUsageStore::new(store.clone());

        let now = Utc::now();
        let mut first = external_usage_record("overview-totals-1", 1.0, 0.8, 1.2);
        first.account_cost_usd = 1.5;
        first.created_at = now.to_rfc3339();
        let mut second = external_usage_record("overview-totals-2", 2.0, 1.6, 2.4);
        second.account_cost_usd = 3.0;
        second.created_at = now.to_rfc3339();
        // 本地记录没有账号计费：不贡献账号成本与原始 token。
        let mut local = usage_record("overview-totals-local", 10);
        local.created_at = now.to_rfc3339();
        usage_store
            .record_batch(vec![first.clone(), second.clone(), local])
            .await
            .unwrap();

        let expected_raw_tokens = [&first, &second]
            .iter()
            .map(|record| usage_record_raw_tokens(record))
            .sum::<i64>();
        assert!(expected_raw_tokens > 0, "fixture must carry raw usage");

        let spec = crate::anthropic::usage::resolve_usage_overview_spec(
            Utc::now(),
            Some("UTC"),
            Some("today"),
            None,
            None,
            Some("hour"),
            Some(10),
        )
        .unwrap();
        let overview = usage_store.dashboard_overview(&spec).await.unwrap();
        assert!(
            (overview.external.account_cost_usd - 4.5).abs() < 1e-9,
            "{overview:?}"
        );
        assert_eq!(overview.external.raw_tokens, expected_raw_tokens);
        // 排行里的“账号”就是账号池：两条外部记录同一个池 #42。
        assert_eq!(overview.top_accounts.len(), 1);
        assert_eq!(overview.top_accounts[0].key, "42");
        assert_eq!(overview.top_accounts[0].label, "backup-a");
        assert_eq!(overview.top_accounts[0].requests, 2);

        // 把列清零后用回填重建，必须得到同样的数字。
        for table in ["usage_rollup_totals", "usage_rollup_time_buckets"] {
            sqlx::query(&format!(
                "UPDATE {table} SET account_cost_usd = 0, account_raw_tokens = 0"
            ))
            .execute(store.pool())
            .await
            .unwrap();
        }
        let scanned = usage_store.backfill_usage_overview_totals().await.unwrap();
        assert_eq!(scanned, 3);
        let rebuilt = usage_store.dashboard_overview(&spec).await.unwrap();
        assert!((rebuilt.external.account_cost_usd - 4.5).abs() < 1e-9);
        assert_eq!(rebuilt.external.raw_tokens, expected_raw_tokens);
        let marker: Option<String> =
            sqlx::query_scalar("SELECT version FROM schema_migrations WHERE version = $1")
                .bind(USAGE_OVERVIEW_TOTALS_BACKFILL_VERSION)
                .fetch_optional(store.pool())
                .await
                .unwrap();
        assert!(marker.is_some(), "backfill must record its marker");

        // 软删一条后合计跟着减少（负向增量同样带着这两列）。
        let cutoff = Utc::now();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        let mut newer = external_usage_record("overview-totals-3", 1.0, 0.8, 1.2);
        newer.account_cost_usd = 0.25;
        newer.created_at = Utc::now().to_rfc3339();
        usage_store.record(newer).await.unwrap();
        let result = usage_store
            .soft_delete_cleanup_batch(cutoff, 10)
            .await
            .unwrap();
        assert_eq!(result.processed_rows, 3);
        let after = usage_store.dashboard_overview(&spec).await.unwrap();
        assert!(
            (after.external.account_cost_usd - 0.25).abs() < 1e-9,
            "{after:?}"
        );

        store.drop_test_schema().await.unwrap();
    }

    /// 账号尝试维度：一条记录两次尝试（一次失败被切走、一次成功）→ 尝试请求 2、尝试错误 1、
    /// 最终成功 1；软删抵消后归零；维护命令重建后恢复。
    /// 拿到响应头就记成 success 的最后一次尝试，流中途出错时要按失败计，
    /// 耗时取整个请求减去前面的尝试，和账号维度同一口径。
    #[test]
    fn attempt_entries_follow_the_final_record_status() {
        let attempt = |pool_id: u64, action: &str, duration_ms: u64| AccountAttempt {
            attempt: 1,
            pool_id,
            pool_name: format!("pool-{pool_id}"),
            outbound_model: None,
            status: Some(200),
            action: action.to_string(),
            duration_ms,
            error_type: None,
            error_message: None,
            raw_upstream_error: None,
        };
        let mut record = external_usage_record("attempt-final", 1.0, 0.8, 1.2);
        record.duration_ms = 2_000;
        record.external_attempts = vec![attempt(7, "retry_next", 300), attempt(42, "success", 120)];

        let summarize = |record: &UsageRecord| {
            usage_rollup_attempt_entries(record, 1)
                .into_iter()
                .map(|(dimension, metrics)| {
                    (
                        dimension.key,
                        metrics.success_requests,
                        metrics.error_requests,
                        metrics.duration_ms_sum,
                    )
                })
                .collect::<Vec<_>>()
        };
        record.status = UsageRecordStatus::Success;
        assert_eq!(
            summarize(&record),
            vec![
                ("7".to_string(), 0, 1, 300),
                ("42".to_string(), 1, 0, 1_700)
            ]
        );
        record.status = UsageRecordStatus::StreamError;
        assert_eq!(
            summarize(&record),
            vec![
                ("7".to_string(), 0, 1, 300),
                ("42".to_string(), 0, 1, 1_700)
            ]
        );
        // 负向增量（软删）同样按最终状态抵消。
        let negative = usage_rollup_attempt_entries(&record, -1);
        assert_eq!(negative[1].1.error_requests, -1);
        assert_eq!(negative[1].1.duration_ms_sum, -1_700);
    }

    #[tokio::test]
    async fn postgres_account_attempt_dimension_counts_failed_attempts_and_reconciles() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
            return;
        };
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;
        let usage_store = PostgresUsageStore::new(store.clone());

        let now = Utc::now();
        let mut record = external_usage_record("attempt-1", 1.0, 0.8, 1.2);
        record.created_at = now.to_rfc3339();
        record.duration_ms = 500;
        record.external_attempts = vec![
            AccountAttempt {
                attempt: 1,
                pool_id: 7,
                pool_name: "flaky".to_string(),
                outbound_model: None,
                status: Some(529),
                action: "retry_next".to_string(),
                duration_ms: 300,
                error_type: Some("overloaded".to_string()),
                error_message: Some("overloaded".to_string()),
                raw_upstream_error: None,
            },
            AccountAttempt {
                attempt: 2,
                pool_id: 42,
                pool_name: "backup-a".to_string(),
                outbound_model: None,
                status: Some(200),
                action: "success".to_string(),
                duration_ms: 200,
                error_type: None,
                error_message: None,
                raw_upstream_error: None,
            },
        ];
        usage_store.record(record).await.unwrap();

        let spec = crate::anthropic::usage::resolve_usage_overview_spec(
            Utc::now(),
            Some("UTC"),
            Some("today"),
            None,
            None,
            Some("hour"),
            Some(10),
        )
        .unwrap();
        let overview = usage_store.dashboard_overview(&spec).await.unwrap();
        let pool = |id: u64| {
            overview
                .external
                .pools
                .iter()
                .find(|pool| pool.pool_id == id)
                .cloned()
                .unwrap_or_else(|| panic!("pool {id} missing: {overview:?}"))
        };
        // 最终落到 #42：最终口径 1 请求 0 错误；尝试口径 1 次成功。
        let winner = pool(42);
        assert_eq!(winner.metrics.requests, 1);
        assert_eq!(winner.metrics.error_requests, 0);
        assert_eq!(winner.attempt_requests, 1);
        assert_eq!(winner.attempt_error_requests, 0);
        assert!((winner.attempt_average_duration_ms - 200.0).abs() < 1e-9);
        assert!((winner.account_cost_usd - 0.0).abs() < 1e-9);
        // 被切走的 #7：最终口径没有请求，尝试口径 1 次失败，错误率 100%。
        let loser = pool(7);
        assert_eq!(loser.pool_name, "flaky");
        assert_eq!(loser.metrics.requests, 0);
        assert_eq!(loser.attempt_requests, 1);
        assert_eq!(loser.attempt_error_requests, 1);
        assert!((loser.attempt_error_rate - 1.0).abs() < 1e-9);
        assert!((loser.attempt_average_duration_ms - 300.0).abs() < 1e-9);
        // 排行里的账号只看最终口径：#7 没有请求不进排行。
        assert_eq!(overview.top_accounts.len(), 1);
        assert_eq!(overview.top_accounts[0].key, "42");

        let attempt_rows = |table: &str| {
            let store = store.clone();
            let sql = format!(
                "SELECT COALESCE(SUM(requests), 0)::bigint, COALESCE(SUM(error_requests), 0)::bigint \
                 FROM {table} WHERE dimension = 'account_attempt'"
            );
            async move {
                sqlx::query_as::<_, (i64, i64)>(&sql)
                    .fetch_one(store.pool())
                    .await
                    .unwrap()
            }
        };
        assert_eq!(attempt_rows("usage_rollup_totals").await, (2, 1));
        assert_eq!(attempt_rows("usage_rollup_time_buckets").await, (2, 1));

        // 维护命令：删掉尝试维度再从明细重建，结果一致。
        let scanned = usage_store.rebuild_usage_route_dimensions().await.unwrap();
        assert_eq!(scanned, 1);
        assert_eq!(attempt_rows("usage_rollup_totals").await, (2, 1));
        assert_eq!(attempt_rows("usage_rollup_time_buckets").await, (2, 1));

        // 软删抵消：负向增量同样走尝试维度，空行被回收。
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        let result = usage_store
            .soft_delete_cleanup_batch(Utc::now(), 10)
            .await
            .unwrap();
        assert_eq!(result.processed_rows, 1);
        assert_eq!(attempt_rows("usage_rollup_totals").await, (0, 0));
        assert_eq!(attempt_rows("usage_rollup_time_buckets").await, (0, 0));
        let after = usage_store.dashboard_overview(&spec).await.unwrap();
        assert!(after.external.pools.is_empty(), "{after:?}");

        store.drop_test_schema().await.unwrap();
    }

    /// 排行行带完整指标（成功数、缓存命中率分子分母、原始计费、平均耗时）；
    /// 账号成本按账号拆到总览账号表；密钥积分限额按账号成本计数。
    #[tokio::test]
    async fn postgres_overview_rank_rows_and_pool_rows_carry_full_metrics() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
            return;
        };
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;
        let usage_store = PostgresUsageStore::new(store.clone());

        let now = Utc::now();
        let mut ok = external_usage_record("rank-ok", 1.0, 0.8, 1.2);
        ok.created_at = now.to_rfc3339();
        ok.duration_ms = 100;
        ok.account_cost_usd = 1.5;
        let mut failed = external_usage_record("rank-fail", 0.5, 0.4, 0.6);
        failed.created_at = now.to_rfc3339();
        failed.duration_ms = 300;
        failed.status = UsageRecordStatus::Error;
        failed.error_type = Some("overloaded".to_string());
        failed.account_cost_usd = 0.5;
        usage_store
            .record_batch(vec![ok.clone(), failed.clone()])
            .await
            .unwrap();

        let spec = crate::anthropic::usage::resolve_usage_overview_spec(
            Utc::now(),
            Some("UTC"),
            Some("today"),
            None,
            None,
            Some("hour"),
            Some(10),
        )
        .unwrap();
        let overview = usage_store.dashboard_overview(&spec).await.unwrap();

        let path = overview
            .top_paths
            .iter()
            .find(|row| row.key == "/cc/v1/messages")
            .expect("path rank row");
        assert_eq!(path.requests, 2);
        assert_eq!(path.success_requests, 1);
        assert_eq!(path.error_requests, 1);
        assert!((path.average_duration_ms - 200.0).abs() < 1e-9);
        assert!((path.original_cost_usd - 1.5).abs() < 1e-9, "{path:?}");
        assert!((path.estimated_cost_usd - 1.8).abs() < 1e-9, "{path:?}");
        // 账号成本（原始成本 × 账号倍率）也按排行维度拆出，用来对照估算计费看差额。
        assert!((path.account_cost_usd - 2.0).abs() < 1e-9, "{path:?}");
        assert_eq!(
            path.uncached_input_tokens,
            i64::from(ok.compat_input_tokens) + i64::from(failed.compat_input_tokens)
        );
        assert_eq!(
            path.cache_read_input_tokens,
            i64::from(ok.cache_read_input_tokens) + i64::from(failed.cache_read_input_tokens)
        );
        assert_eq!(
            path.cache_creation_input_tokens,
            i64::from(ok.cache_creation_input_tokens)
                + i64::from(failed.cache_creation_input_tokens)
        );
        // 模型 / 密钥页签同一套列。
        let model = overview.top_models.first().expect("model row");
        assert_eq!(model.success_requests, 1);
        assert!((model.original_cost_usd - 1.5).abs() < 1e-9);
        assert!((model.account_cost_usd - 2.0).abs() < 1e-9, "{model:?}");
        let key = overview
            .top_keys
            .iter()
            .find(|row| row.key == "request-key-external")
            .expect("key row");
        assert_eq!(key.requests, 2);
        assert!((key.average_duration_ms - 200.0).abs() < 1e-9);
        assert!((key.account_cost_usd - 2.0).abs() < 1e-9, "{key:?}");
        let account_rank = overview
            .top_accounts
            .iter()
            .find(|row| row.key == "42")
            .expect("account rank row");
        assert!(
            (account_rank.account_cost_usd - 2.0).abs() < 1e-9,
            "{account_rank:?}"
        );
        // 账号表：账号成本按账号拆出。
        let pool = overview
            .external
            .pools
            .iter()
            .find(|pool| pool.pool_id == 42)
            .expect("pool row");
        assert!((pool.account_cost_usd - 2.0).abs() < 1e-9, "{pool:?}");
        // 时序：成功数与平均耗时。
        let point = overview
            .series
            .iter()
            .find(|point| point.all.requests > 0)
            .expect("series point");
        assert_eq!(point.all.success_requests, 1);
        assert!((point.all.average_duration_ms - 200.0).abs() < 1e-9);

        // 密钥限额的积分槽位 = 账号成本之和。
        let totals = store
            .request_key_usage_since("request-key-external", None)
            .await
            .unwrap();
        assert!((totals.credits - 2.0).abs() < 1e-9, "{totals:?}");
        assert!((totals.cost_usd - 1.8).abs() < 1e-9, "{totals:?}");
        let since_now = store
            .request_key_usage_since("request-key-external", Some(now - ChronoDuration::hours(2)))
            .await
            .unwrap();
        assert!((since_now.credits - 2.0).abs() < 1e-9, "{since_now:?}");

        store.drop_test_schema().await.unwrap();
    }

    /// 路由页签：入口维度按路由前缀合并出完整指标与成本，时序按路由拆分，
    /// 延迟分位与明细筛选用同一个前缀口径（自定义路由名含 v1 也不截错）。
    #[tokio::test]
    async fn postgres_overview_groups_endpoints_by_route_prefix() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
            return;
        };
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;
        let usage_store = PostgresUsageStore::new(store.clone());

        let now = Utc::now();
        let record = |id: &str, endpoint: &str, duration_ms: u64, account_cost: f64| {
            let mut record = external_usage_record(id, 1.0, 0.8, 1.2);
            record.created_at = now.to_rfc3339();
            record.endpoint = endpoint.to_string();
            record.duration_ms = duration_ms;
            record.account_cost_usd = account_cost;
            record
        };
        let cc_messages = record("route-cc-1", "/cc/v1/messages", 100, 1.5);
        let mut cc_failed = record("route-cc-2", "/cc/v1/messages/count_tokens", 300, 0.5);
        cc_failed.status = UsageRecordStatus::Error;
        cc_failed.error_type = Some("overloaded".to_string());
        let default_route = record("route-default", "/v1/messages", 50, 1.0);
        let custom = record("route-custom", "/dfcache/v1/v1/messages", 70, 1.0);
        usage_store
            .record_batch(vec![
                cc_messages.clone(),
                cc_failed.clone(),
                default_route.clone(),
                custom.clone(),
            ])
            .await
            .unwrap();

        let spec = crate::anthropic::usage::resolve_usage_overview_spec(
            Utc::now(),
            Some("UTC"),
            Some("today"),
            None,
            None,
            Some("hour"),
            Some(10),
        )
        .unwrap();
        let overview = usage_store.dashboard_overview(&spec).await.unwrap();
        let route = |key: &str| {
            overview
                .routes
                .iter()
                .find(|row| row.route == key)
                .unwrap_or_else(|| panic!("route {key} missing: {:?}", overview.routes))
                .clone()
        };
        assert_eq!(overview.routes.len(), 3, "{:?}", overview.routes);
        let cc = route("/cc");
        assert_eq!(
            cc.endpoints,
            vec![
                "/cc/v1/messages".to_string(),
                "/cc/v1/messages/count_tokens".to_string()
            ]
        );
        assert_eq!(cc.metrics.requests, 2);
        assert_eq!(cc.metrics.success_requests, 1);
        assert_eq!(cc.metrics.error_requests, 1);
        assert!((cc.metrics.average_duration_ms - 200.0).abs() < 1e-9);
        assert!((cc.account_cost_usd - 2.0).abs() < 1e-9, "{cc:?}");
        let billing = cc_messages.account_billing.as_ref().expect("billing");
        assert!(
            (cc.raw_cost_usd - billing.raw_cost_usd * 2.0).abs() < 1e-9,
            "{cc:?}"
        );
        assert!(
            (cc.billable_cost_usd - billing.billable_cost_usd * 2.0).abs() < 1e-9,
            "{cc:?}"
        );
        assert_eq!(route("/").metrics.requests, 1);
        assert_eq!(route("/dfcache/v1").metrics.requests, 1);
        // 路由合计与全局一致：每条记录只落一个路由。
        let routed_requests: usize = overview.routes.iter().map(|row| row.metrics.requests).sum();
        assert_eq!(routed_requests, overview.totals.all.requests);

        // 时序：分组按可计费降序，/cc 两条记录排第一；每个点按路由拆分。
        assert_eq!(
            overview
                .series_groups
                .routes
                .first()
                .map(|group| group.key.as_str()),
            Some("/cc"),
            "{:?}",
            overview.series_groups.routes
        );
        let point = overview
            .series
            .iter()
            .find(|point| point.all.requests > 0)
            .expect("series point");
        let cc_point = point
            .routes
            .iter()
            .find(|item| item.key == "/cc")
            .expect("cc breakdown");
        assert_eq!(cc_point.metrics.requests, 2);
        assert_eq!(
            point
                .routes
                .iter()
                .map(|item| item.metrics.requests)
                .sum::<usize>(),
            4
        );

        // 延迟分位：同一前缀口径。
        let latency = usage_store.route_latency(&spec).await.unwrap();
        let latency_cc = latency
            .routes
            .iter()
            .find(|row| row.route == "/cc")
            .expect("cc latency");
        assert_eq!(latency_cc.samples, 2);
        assert!(
            (latency_cc.p50_duration_ms - 200.0).abs() < 1e-9,
            "{latency_cc:?}"
        );
        let mut latency_keys = latency
            .routes
            .iter()
            .map(|row| row.route.clone())
            .collect::<Vec<_>>();
        latency_keys.sort();
        assert_eq!(latency_keys, vec!["/", "/cc", "/dfcache/v1"]);

        // 明细筛选：按前缀精确匹配，默认路由不吞掉其它路由。
        for (route_filter, expected) in [
            ("/cc", vec!["route-cc-1", "route-cc-2"]),
            ("/", vec!["route-default"]),
            ("/dfcache/v1", vec!["route-custom"]),
            ("/dfcache/v1/", vec!["route-custom"]),
            // /dfcache/v1/v1/messages 的 LIKE 也能命中 "/dfcache/v1/%"，必须靠精确前缀排除。
            ("/dfcache", vec![]),
            ("/ha", vec![]),
        ] {
            let page = usage_store
                .query_page(
                    UsageRecordQuery {
                        route: Some(route_filter.to_string()),
                        ..UsageRecordQuery::default()
                    },
                    1,
                    50,
                )
                .await
                .unwrap();
            let mut ids = page
                .records
                .iter()
                .map(|record| record.id.clone())
                .collect::<Vec<_>>();
            ids.sort();
            assert_eq!(ids, expected, "route filter {route_filter}");
        }

        store.drop_test_schema().await.unwrap();
    }

    /// 准入拒绝按请求密钥单独计数；普通错误不算；软删抵消后归零。
    #[tokio::test]
    async fn postgres_request_key_rejection_dimension_counts_only_rejections() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
            return;
        };
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;
        let usage_store = PostgresUsageStore::new(store.clone());

        let now = Utc::now();
        let rejection = |id: &str| {
            let mut record = usage_record(id, 0);
            record.created_at = now.to_rfc3339();
            record.status = UsageRecordStatus::Error;
            record.error_type = Some("request_rejection".to_string());
            record.request_api_key_id = Some("key-limited".to_string());
            record
        };
        let mut plain_error = usage_record("plain-error", 0);
        plain_error.created_at = now.to_rfc3339();
        plain_error.status = UsageRecordStatus::Error;
        plain_error.error_type = Some("overloaded".to_string());
        plain_error.request_api_key_id = Some("key-limited".to_string());
        let mut other_key = rejection("rejected-other");
        other_key.request_api_key_id = Some("key-other".to_string());
        usage_store
            .record_batch(vec![
                rejection("rejected-1"),
                rejection("rejected-2"),
                plain_error,
                other_key,
            ])
            .await
            .unwrap();

        let rejections = store
            .request_key_rejections_since(now - ChronoDuration::hours(24))
            .await
            .unwrap();
        assert_eq!(rejections.get("key-limited").copied(), Some(2));
        assert_eq!(rejections.get("key-other").copied(), Some(1));
        assert_eq!(rejections.len(), 2);

        // 时间窗之外不算。
        let future = store
            .request_key_rejections_since(now + ChronoDuration::hours(2))
            .await
            .unwrap();
        assert!(future.is_empty());

        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        let result = usage_store
            .soft_delete_cleanup_batch(Utc::now(), 10)
            .await
            .unwrap();
        assert_eq!(result.processed_rows, 4);
        let after = store
            .request_key_rejections_since(now - ChronoDuration::hours(24))
            .await
            .unwrap();
        assert!(after.is_empty(), "{after:?}");

        store.drop_test_schema().await.unwrap();
    }

    /// 按账号的耗时 / 首字分位数直接扫明细；没有首字延迟的账号对应列为空。
    #[tokio::test]
    async fn postgres_account_latency_percentiles_come_from_detail_rows() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
            return;
        };
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;
        let usage_store = PostgresUsageStore::new(store.clone());

        let now = Utc::now();
        let mut records = Vec::new();
        for (index, duration) in [100_u64, 200, 300, 400, 500].into_iter().enumerate() {
            let mut record = external_usage_record(&format!("latency-{index}"), 1.0, 0.8, 1.2);
            record.created_at = now.to_rfc3339();
            record.duration_ms = duration;
            record.first_token_latency_ms = Some(duration / 2);
            records.push(record);
        }
        // 另一个账号：没有首字延迟。
        let mut other = external_usage_record("latency-other", 1.0, 0.8, 1.2);
        other.created_at = now.to_rfc3339();
        other.account_id = Some(9);
        other.account_name = Some("quiet".to_string());
        other.duration_ms = 1_000;
        other.first_token_latency_ms = None;
        records.push(other);
        // 本地记录没有账号，不参与。
        let mut local = usage_record("latency-local", 10);
        local.created_at = now.to_rfc3339();
        records.push(local);
        usage_store.record_batch(records).await.unwrap();

        let spec = crate::anthropic::usage::resolve_usage_overview_spec(
            Utc::now(),
            Some("UTC"),
            Some("today"),
            None,
            None,
            Some("hour"),
            Some(10),
        )
        .unwrap();
        let response = usage_store.account_latency(&spec, None).await.unwrap();
        assert_eq!(response.accounts.len(), 2, "{response:?}");
        let main = response
            .accounts
            .iter()
            .find(|row| row.account_id == 42)
            .expect("account 42");
        assert_eq!(main.account_name, "backup-a");
        assert_eq!(main.samples, 5);
        assert!((main.p50_duration_ms - 300.0).abs() < 1e-9, "{main:?}");
        assert!((main.p95_duration_ms - 480.0).abs() < 1e-6, "{main:?}");
        assert!((main.p99_duration_ms - 496.0).abs() < 1e-6, "{main:?}");
        assert_eq!(main.first_token_samples, 5);
        assert!((main.p50_first_token_latency_ms.unwrap() - 150.0).abs() < 1e-9);
        let quiet = response
            .accounts
            .iter()
            .find(|row| row.account_id == 9)
            .expect("account 9");
        assert_eq!(quiet.samples, 1);
        assert_eq!(quiet.first_token_samples, 0);
        assert!(quiet.p95_first_token_latency_ms.is_none());
        assert!((quiet.p50_duration_ms - 1_000.0).abs() < 1e-9);

        // 按账号过滤。
        let filtered = usage_store.account_latency(&spec, Some(9)).await.unwrap();
        assert_eq!(filtered.accounts.len(), 1);
        assert_eq!(filtered.accounts[0].account_id, 9);

        store.drop_test_schema().await.unwrap();
    }

    /// 质量采样快照：写入、按账号读取（升序）、过期清理。
    #[tokio::test]
    async fn postgres_account_quality_samples_round_trip_and_expire() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
            return;
        };
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;

        let now = Utc::now();
        let sample = |minutes_ago: i64, error_rate: f64| AccountQualitySampleRow {
            account_id: 42,
            sampled_at: now - ChronoDuration::minutes(minutes_ago),
            recent_error_rate: error_rate,
            ttft_ewma_ms: Some(120.0),
            latency_ewma_ms: Some(900.0),
            sample_count: 10,
        };
        store
            .record_account_quality_sample(&sample(10, 0.2))
            .await
            .unwrap();
        store
            .record_account_quality_sample(&sample(5, 0.1))
            .await
            .unwrap();
        // 过期点（8 天前）与其它账号的点。
        let mut old = sample(8 * 24 * 60, 0.9);
        old.ttft_ewma_ms = None;
        store.record_account_quality_sample(&old).await.unwrap();
        let mut other = sample(1, 0.0);
        other.account_id = 7;
        store.record_account_quality_sample(&other).await.unwrap();

        let until = now + ChronoDuration::minutes(1);
        let history = store
            .account_quality_history(42, now - ChronoDuration::hours(24), until, 60)
            .await
            .unwrap();
        assert_eq!(history.len(), 2, "{history:?}");
        assert!(history[0].sampled_at < history[1].sampled_at, "ascending");
        assert!((history[0].recent_error_rate - 0.2).abs() < 1e-9);
        assert!((history[1].recent_error_rate - 0.1).abs() < 1e-9);
        assert_eq!(history[1].ttft_ewma_ms, Some(120.0));

        // 同一时刻重复写入是更新不是报错。
        let mut updated = sample(5, 0.3);
        updated.sample_count = 11;
        store.record_account_quality_sample(&updated).await.unwrap();
        let history = store
            .account_quality_history(42, now - ChronoDuration::hours(24), until, 60)
            .await
            .unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[1].sample_count, 11);

        // 长窗口降采样：同一个桶里的点取平均，桶起点作为时间。
        let bucket_secs = 864_000_u32;
        let bucket_of = |minutes_ago: i64| {
            (now - ChronoDuration::minutes(minutes_ago)).timestamp() / i64::from(bucket_secs)
        };
        let coarse = store
            .account_quality_history(42, now - ChronoDuration::hours(24), until, bucket_secs)
            .await
            .unwrap();
        if bucket_of(10) == bucket_of(5) {
            assert_eq!(coarse.len(), 1, "{coarse:?}");
            assert!(
                (coarse[0].recent_error_rate - 0.25).abs() < 1e-9,
                "{coarse:?}"
            );
            assert_eq!(coarse[0].sample_count, 11);
            assert_eq!(coarse[0].sampled_at.timestamp() % i64::from(bucket_secs), 0);
        } else {
            assert_eq!(coarse.len(), 2, "{coarse:?}");
        }
        // 窗口终点之后的点不返回。
        let bounded = store
            .account_quality_history(
                42,
                now - ChronoDuration::hours(24),
                now - ChronoDuration::minutes(7),
                60,
            )
            .await
            .unwrap();
        assert_eq!(bounded.len(), 1, "{bounded:?}");

        let removed = store
            .delete_expired_account_quality_samples(
                std::time::Duration::from_secs(7 * 24 * 60 * 60),
                100,
            )
            .await
            .unwrap();
        assert_eq!(removed, 1);
        let all: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM account_quality_samples")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(all, 3);

        store.drop_test_schema().await.unwrap();
    }

    /// 用量明细保留只删 usage_records（含墓碑），汇总与总览保持不变。
    #[tokio::test]
    async fn postgres_usage_retention_deletes_detail_but_keeps_rollups() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
            return;
        };
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;
        let usage_store = PostgresUsageStore::new(store.clone());

        let old_created = Utc::now() - chrono::Duration::days(10);
        let mut records = Vec::new();
        for index in 0..5 {
            let mut record = usage_record(&format!("retention-old-{index}"), 100);
            record.created_at = old_created.to_rfc3339();
            records.push(record);
        }
        let mut fresh = usage_record("retention-fresh", 100);
        fresh.created_at = Utc::now().to_rfc3339();
        records.push(fresh);
        usage_store.record_batch(records).await.unwrap();
        let summary_before = usage_store.summary(500).await.unwrap();
        assert_eq!(summary_before.total_requests, 6);

        // 7 天保留，批大小 2：三批删完 5 条旧的，新的留下。
        let retention = std::time::Duration::from_secs(7 * 24 * 60 * 60);
        let mut removed_total = 0;
        for _ in 0..4 {
            let removed = usage_store
                .delete_expired_usage_records(retention, 2)
                .await
                .unwrap();
            removed_total += removed;
            if removed < 2 {
                break;
            }
        }
        assert_eq!(removed_total, 5);
        let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*)::bigint FROM usage_records")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(remaining, 1);
        let summary_after = usage_store.summary(500).await.unwrap();
        assert_eq!(
            summary_after.total_requests, 6,
            "rollups must keep history after detail retention"
        );
        assert_eq!(
            usage_store
                .delete_expired_usage_records(retention, 2)
                .await
                .unwrap(),
            0
        );

        // 会话维度汇总按最后活跃时间裁剪：把两条会话行的 updated_at 做旧，其余不动。
        for key in ["conv-stale-1", "conv-stale-2", "conv-live"] {
            sqlx::query(
                r#"
                INSERT INTO usage_rollup_totals (dimension, dimension_key, requests, updated_at)
                VALUES ('conversation', $1, 1, now())
                ON CONFLICT (dimension, dimension_key) DO NOTHING
                "#,
            )
            .bind(key)
            .execute(store.pool())
            .await
            .unwrap();
        }
        sqlx::query(
            "UPDATE usage_rollup_totals SET updated_at = now() - interval '30 days' WHERE dimension = 'conversation' AND dimension_key LIKE 'conv-stale-%'",
        )
        .execute(store.pool())
        .await
        .unwrap();
        let pruned = usage_store
            .delete_stale_conversation_rollups(retention, 10)
            .await
            .unwrap();
        assert_eq!(pruned, 2);
        let live: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM usage_rollup_totals WHERE dimension = 'conversation' AND dimension_key = 'conv-live'",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(live, 1, "recently active conversations stay");
        let global: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM usage_rollup_totals WHERE dimension = 'global'",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(global, 1, "other dimensions are untouched");

        store.drop_test_schema().await.unwrap();
    }

    /// 明细保留任务删明细时保留汇总；之后"从明细重建"的维护命令必须拒绝执行，
    /// 否则已删明细对应的历史统计会被永久抹掉。
    #[tokio::test]
    async fn postgres_rebuild_commands_refuse_after_retention_deleted_details() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
            return;
        };
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        clean(&store).await;
        let usage_store = PostgresUsageStore::new(store.clone());

        let mut old_record = usage_record("rebuild-guard-old", 800);
        old_record.created_at = (Utc::now() - chrono::Duration::days(2)).to_rfc3339();
        let mut fresh_record = usage_record("rebuild-guard-fresh", 900);
        // 留足余量：下面先跑两个重建命令，负载高时会超过 1 秒，清理截止时间不能扫到这条。
        fresh_record.created_at = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        usage_store
            .record_batch(vec![old_record, fresh_record])
            .await
            .unwrap();

        // 明细完整时两个命令照常执行。
        assert_eq!(
            usage_store.rebuild_usage_route_dimensions().await.unwrap(),
            2
        );
        assert_eq!(
            usage_store.backfill_usage_overview_totals().await.unwrap(),
            2
        );

        let result = usage_store
            .hard_delete_cleanup_batch_preserving_summary(Utc::now(), 10)
            .await
            .unwrap();
        assert_eq!(result.processed_rows, 1);

        for err in [
            usage_store
                .rebuild_usage_route_dimensions()
                .await
                .unwrap_err(),
            usage_store
                .backfill_usage_overview_totals()
                .await
                .unwrap_err(),
        ] {
            let message = err.to_string();
            assert!(message.contains("已拒绝执行"), "{message}");
            assert!(message.contains("2") && message.contains("1"), "{message}");
        }
        // 汇总没有被动过。
        let summary = usage_store.summary(500).await.unwrap();
        assert_eq!(summary.total_requests, 2);

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_physical_cleanup_preserves_rollups_for_active_rows_for_three_rounds() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
            return;
        };
        let store = Arc::new(PostgresStore::connect_test(&config).await.unwrap());
        let usage_store = PostgresUsageStore::new(store.clone());

        for round in 0..3 {
            clean(&store).await;
            // 截止时间先定下来：新旧记录都相对它取时间，清理也用它。之前"新"记录只比现在
            // 晚 1 秒、清理截止取执行时的现在，负载高时中间的写入与汇总查询超过 1 秒，
            // 新记录也会被删掉。
            let cutoff = Utc::now();
            let mut old_record = usage_record(&format!("physical-old-round-{round}"), 800);
            old_record.created_at = (cutoff - chrono::Duration::days(2)).to_rfc3339();
            old_record.estimated_cost_usd = 0.31;
            old_record.original_cost_usd = 0.36;

            let mut fresh_record = usage_record(&format!("physical-fresh-round-{round}"), 900);
            fresh_record.created_at = (cutoff + chrono::Duration::hours(1)).to_rfc3339();
            fresh_record.estimated_cost_usd = 0.17;
            fresh_record.original_cost_usd = 0.19;
            usage_store
                .record_batch(vec![old_record, fresh_record])
                .await
                .unwrap();

            let before = usage_store.summary(500).await.unwrap();
            assert_eq!(before.total_requests, 2, "round {round}: summary before");
            assert!((before.total_estimated_cost_usd - 0.48).abs() < 1e-12);

            let result = usage_store
                .hard_delete_cleanup_batch_preserving_summary(cutoff, 10)
                .await
                .unwrap();
            assert_eq!(
                result.processed_rows, 1,
                "round {round}: physical row count"
            );
            assert_eq!(result.has_remaining, Some(false), "round {round}");

            let physical_rows: i64 =
                sqlx::query_scalar("SELECT COUNT(*)::bigint FROM usage_records")
                    .fetch_one(store.pool())
                    .await
                    .unwrap();
            assert_eq!(physical_rows, 1, "round {round}: fresh row remains");

            let after = usage_store.summary(500).await.unwrap();
            assert_eq!(
                after.total_requests, 2,
                "round {round}: physical cleanup preserves summary"
            );
            assert!((after.total_estimated_cost_usd - 0.48).abs() < 1e-12);
        }

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_cleanup_recomputes_global_duration_max_for_three_rounds() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
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
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
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
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
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
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
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
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
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
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
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
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
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
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
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

    #[tokio::test]
    async fn postgres_usage_lifecycle_fence_requires_offline_maintenance() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
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
        ] {
            assert!(
                USAGE_LEGACY_COST_FIELD_BACKFILL_SQL.contains(expected),
                "legacy cost backfill maintenance SQL should contain expected statement: {expected}"
            );
        }
    }

    #[tokio::test]
    async fn postgres_repeated_startup_migration_preserves_unchanged_inline_marker_time() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
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
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
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
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
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
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
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
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
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
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
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

        let mut first_boundary = usage_record("dashboard-first-boundary", 2_000);
        first_boundary.created_at = (base + chrono::Duration::minutes(30)).to_rfc3339();
        first_boundary.duration_ms = 30;

        let mut before_full_hour = usage_record("dashboard-before-full-hour", 0);
        before_full_hour.created_at =
            (base + chrono::Duration::minutes(59) + chrono::Duration::seconds(59)).to_rfc3339();
        before_full_hour.status = UsageRecordStatus::Error;
        before_full_hour.usage_source = UsageSource::RequestEstimate;
        before_full_hour.duration_ms = 40;

        let mut full_hour_start = usage_record("dashboard-full-hour-start", 2_000);
        full_hour_start.created_at = (base + chrono::Duration::hours(1)).to_rfc3339();
        full_hour_start.duration_ms = 50;

        let mut full_hour_external =
            external_usage_record("dashboard-full-hour-external", 0.010, 0.006, 0.008);
        full_hour_external.created_at =
            (base + chrono::Duration::hours(1) + chrono::Duration::minutes(30)).to_rfc3339();

        let mut tail_legacy = usage_record("dashboard-tail-legacy", 2_000);
        tail_legacy.created_at =
            (base + chrono::Duration::hours(2) + chrono::Duration::minutes(30)).to_rfc3339();
        tail_legacy.status = UsageRecordStatus::StreamError;
        tail_legacy.usage_source = UsageSource::ContextEstimate;
        tail_legacy.estimated_cost_usd = 0.123;
        tail_legacy.original_cost_usd = 0.0;
        tail_legacy.duration_ms = 60;

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
        assert_eq!(by_key["exact-hour"].summary.total_requests, 2);
        assert_eq!(by_key["exact-hour"].summary.high_cache_requests, 2);
        assert_eq!(by_key["half-hour"].summary.total_requests, 3);

        let series = usage_store.dashboard_series(&specs).await.unwrap();
        let series_by_key = series
            .iter()
            .map(|point| (point.key.as_str(), point))
            .collect::<HashMap<_, _>>();
        assert_eq!(series_by_key["same-hour"].requests, 2);
        assert_eq!(series_by_key["cross-hour"].requests, 6);
        assert_eq!(series_by_key["exact-hour"].requests, 2);
        assert_eq!(series_by_key["half-hour"].requests, 3);

        let top_credentials = usage_store
            .dashboard_top_aggregates_for_window(&[specs[1].clone()], DashboardTopGroup::Credential)
            .await
            .unwrap();
        assert_eq!(top_credentials.items[0].key, "7");

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
            .dashboard_account_billing_by_pool(&specs)
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
            by_key["cross-hour"].summary.account_billing.requests,
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
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
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
                    requests, account_requests, account_priced_requests,
                    account_raw_cost_usd, account_shaped_cost_usd,
                    account_uplifted_cost_usd, account_profit_usd,
                    account_reported_cost_usd, account_billable_cost_usd
                )
                VALUES ($1, 'account', '42', 'legacy-pool',
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
            .dashboard_account_billing_by_pool(&specs)
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
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
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

    fn external_usage_snapshot(
        input_tokens: i32,
        output_tokens: i32,
        cache_read_input_tokens: i32,
        cache_creation_input_tokens: i32,
    ) -> AccountUsageSnapshot {
        AccountUsageSnapshot {
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
            account_cost_usd: 0.0,
            id: id.to_string(),
            created_at: Utc::now().to_rfc3339(),
            endpoint: "/cc/v1/messages".to_string(),
            stream: id.ends_with('0'),
            model: "claude-sonnet-4-5".to_string(),
            requested_max_tokens: None,
            downstream_stop_reason: None,
            upstream_model: Some("claude-sonnet-4-5".to_string()),
            external_outbound_model: Some("claude-sonnet-4-5".to_string()),
            upstream_response_model: None,
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

            pricing_available: true,
            pricing_model: Some("claude-sonnet-4-5".to_string()),
            duration_ms: 50,
            first_token_latency_ms: None,
            response_latency_ms: Some(50),
            latency_trace: None,
            simulated: false,

            route_kind: Some(UsageRouteKind::Account),
            route_subtype: Some(UsageRouteSubtype::Account),

            account_id: Some(42),
            account_name: Some("backup-a".to_string()),
            external_attempts: Vec::new(),
            usage_projection_applied: Some(true),
            account_billing: Some(AccountBilling {
                request_input_tokens: None,
                upstream_response_model: None,
                raw_cost_usage: raw_usage,
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
                raw_usage_source: Default::default(),
                usage_projection_mode: "current_path_policy".to_string(),
                stream_response_mode: None,
                usage_estimated: false,
                usage_estimate_reason: None,
                usage_candidate_path: None,
                body_usage_projection_applied: true,
            }),

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
    async fn postgres_account_list_and_get_preserve_body_modes() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;

        let created = store
            .create_account(CreateAccountRequest {
                tags: Vec::new(),
                proxy_url: None,
                cost_multiplier: 1.0,
                overrides: Default::default(),
                name: "raw-pool".to_string(),
                base_url: "https://example.com".to_string(),
                api_key: "sk-test".to_string(),
                auth_type: AccountAuthType::Bearer,
                header_profile: AccountHeaderProfile::Generic,
                append_beta_query: false,
                header_overrides: HashMap::new(),
                wire_profile: AccountWireProfile::Default,
                tls_profile: AccountTlsProfile::Default,
                enabled: true,
                priority: 1,
                max_concurrent_requests: 2,
                raw_usage_source: Default::default(),
                usage_projection_mode: UsageProjectionMode::PassThrough,
                stream_response_mode: Some(StreamResponseMode::EventPassthrough),
                auto_disable_policy: AutoDisablePolicy::Inherit,
                pre_output_stream_retry_mode: StreamRetryMode::Enabled,
                model_mapping_mode: ModelMappingMode::PassthroughMapping,
                model_mapping_require_match: false,
                model_mapping_rules: vec![ModelMappingRule {
                    enabled: true,
                    source: "client-model".to_string(),
                    target: "mapped-model".to_string(),
                    kind: Default::default(),
                    note: None,
                }],
                model_filter_mode: Default::default(),
                supported_models: Vec::new(),
                route_mode: RouteRuleMode::AllowList,
                route_rules: vec!["/CC".to_string(), "/ha".to_string()],
                notes: None,
            })
            .await
            .unwrap();

        assert_eq!(
            created.stream_response_mode,
            Some(StreamResponseMode::EventPassthrough)
        );
        assert_eq!(
            created.pre_output_stream_retry_mode,
            StreamRetryMode::Enabled
        );
        assert_eq!(created.route_mode, RouteRuleMode::AllowList);
        assert_eq!(
            created.route_rules,
            vec!["/cc".to_string(), "/ha".to_string()]
        );

        let listed = store.list_accounts(false).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].stream_response_mode,
            Some(StreamResponseMode::EventPassthrough)
        );
        assert_eq!(
            listed[0].pre_output_stream_retry_mode,
            StreamRetryMode::Enabled
        );

        let loaded = store
            .get_account(created.id, false)
            .await
            .unwrap()
            .expect("pool should exist");
        assert_eq!(
            loaded.stream_response_mode,
            Some(StreamResponseMode::EventPassthrough)
        );
        assert_eq!(
            loaded.pre_output_stream_retry_mode,
            StreamRetryMode::Enabled
        );
        assert_eq!(loaded.route_mode, RouteRuleMode::AllowList);
        assert_eq!(
            loaded.route_rules,
            vec!["/cc".to_string(), "/ha".to_string()]
        );

        let updated = store
            .update_account(
                created.id,
                UpdateAccountRequest {
                    stream_response_mode: Some(Some(StreamResponseMode::EventPassthrough)),

                    pre_output_stream_retry_mode: Some(StreamRetryMode::Disabled),
                    route_mode: Some(RouteRuleMode::DenyList),
                    route_rules: Some(vec!["/V1".to_string()]),
                    ..UpdateAccountRequest::default()
                },
            )
            .await
            .unwrap()
            .expect("pool should update");
        assert_eq!(
            updated.stream_response_mode,
            Some(StreamResponseMode::EventPassthrough)
        );
        assert_eq!(
            updated.pre_output_stream_retry_mode,
            StreamRetryMode::Disabled
        );
        assert_eq!(updated.route_mode, RouteRuleMode::DenyList);
        assert_eq!(updated.route_rules, vec!["/v1".to_string()]);

        let inherited = store
            .update_account(
                created.id,
                UpdateAccountRequest {
                    stream_response_mode: Some(None),
                    ..UpdateAccountRequest::default()
                },
            )
            .await
            .unwrap()
            .expect("pool should clear stream override");
        assert_eq!(inherited.stream_response_mode, None);
        assert_eq!(
            inherited.pre_output_stream_retry_mode,
            StreamRetryMode::Disabled
        );
        assert_eq!(inherited.route_mode, RouteRuleMode::DenyList);
        assert_eq!(inherited.route_rules, vec!["/v1".to_string()]);

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn postgres_rolls_up_account_billing_for_large_samples_and_removes_after_cleanup() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
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
        assert_eq!(summary.account_billing.requests, 1000);
        assert_eq!(summary.account_billing.priced_requests, 1000);
        assert_eq!(summary.account_billing.unpriced_requests, 0);
        assert_eq!(summary.account_billing.cost_floor_applied_requests, 500);
        assert!((summary.account_billing.raw_cost_usd - 7.5).abs() < 0.000001);
        assert!((summary.account_billing.shaped_cost_usd - 6.5).abs() < 0.000001);
        assert!((summary.account_billing.uplifted_cost_usd - 8.5).abs() < 0.000001);
        assert!((summary.account_billing.profit_usd - 1.0).abs() < 0.000001);
        assert!((summary.account_billing.reported_cost_usd - 8.5).abs() < 0.000001);
        assert!((summary.account_billing.billable_cost_usd - 8.5).abs() < 0.000001);
        assert!((summary.account_billing.cost_floor_delta_usd - 1.0).abs() < 0.000001);
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
        assert_eq!(today.summary.account_billing.requests, 1000);
        assert_eq!(
            today.summary.account_billing.cost_floor_applied_requests,
            500
        );
        assert!((today.summary.account_billing.billable_cost_usd - 8.5).abs() < 0.000001);
        assert!((today.summary.account_billing.profit_usd - 1.0).abs() < 0.000001);

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
        assert_eq!(cleared_summary.account_billing.requests, 0);
        assert_eq!(cleared_summary.account_billing.priced_requests, 0);
        assert!(cleared_summary.account_billing.billable_cost_usd.abs() < 0.000001);
        assert!(cleared_summary.account_billing.profit_usd.abs() < 0.000001);
        assert!(cleared_summary.total_estimated_cost_usd.abs() < 0.000001);

        store.drop_test_schema().await.unwrap();
    }
    #[tokio::test]
    async fn postgres_raw_usage_source_default_update_copy_reload_and_schema_guard() {
        let Some(config) = test_config() else {
            return;
        };
        let store = PostgresStore::connect_test(&config).await.unwrap();
        let request: CreateAccountRequest = serde_json::from_value(serde_json::json!({
            "name":"raw-source", "baseUrl":"https://example.com", "apiKey":"test-only"
        }))
        .unwrap();
        let original = store.create_account_unmasked(request).await.unwrap();
        assert_eq!(original.raw_usage_source, RawUsageSource::LegacyMax);
        let enabled = store
            .update_account_unmasked(
                original.id,
                serde_json::from_value(serde_json::json!({"rawUsageSource":"upstream_last"}))
                    .unwrap(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(enabled.raw_usage_source, RawUsageSource::UpstreamLast);
        let unchanged = store
            .update_account_unmasked(original.id, UpdateAccountRequest::default())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(unchanged.raw_usage_source, RawUsageSource::UpstreamLast);
        assert!(
            store
                .list_account_eligibility()
                .await
                .unwrap()
                .iter()
                .any(|p| p.id == original.id)
        );
        assert_eq!(
            store
                .get_account(original.id, false)
                .await
                .unwrap()
                .unwrap()
                .raw_usage_source,
            RawUsageSource::UpstreamLast
        );
        let hybrid = store
            .update_account_unmasked(
                original.id,
                serde_json::from_value(
                    serde_json::json!({"rawUsageSource":"upstream_last_max_cost"}),
                )
                .unwrap(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(hybrid.raw_usage_source, RawUsageSource::UpstreamLastMaxCost);
        sqlx::query("ALTER TABLE accounts DROP COLUMN raw_usage_source")
            .execute(&store.pool)
            .await
            .unwrap();
        assert!(
            store
                .verify_required_schema_compatibility(false)
                .await
                .unwrap_err()
                .to_string()
                .contains("raw_usage_source")
        );
        store.drop_test_schema().await.unwrap();
    }
    #[tokio::test]
    async fn postgres_raw_usage_source_migrates_existing_two_value_constraint() {
        let Some(config) = test_config() else {
            return;
        };
        let store = PostgresStore::connect_test(&config).await.unwrap();
        sqlx::raw_sql("ALTER TABLE accounts DROP CONSTRAINT accounts_raw_usage_source_check; ALTER TABLE accounts ADD CONSTRAINT accounts_raw_usage_source_check CHECK(raw_usage_source IN ('legacy_max','upstream_last'))").execute(&store.pool).await.unwrap();
        assert!(
            store
                .verify_required_schema_compatibility(false)
                .await
                .unwrap_err()
                .to_string()
                .contains("raw_usage_source_check")
        );
        store.migrate_with_options(false).await.unwrap();
        let request:CreateAccountRequest=serde_json::from_value(serde_json::json!({"name":"hybrid-migrated","baseUrl":"https://example.com","apiKey":"test-only","rawUsageSource":"upstream_last_max_cost"})).unwrap();
        assert_eq!(
            store
                .create_account(request)
                .await
                .unwrap()
                .raw_usage_source,
            RawUsageSource::UpstreamLastMaxCost
        );
        store.drop_test_schema().await.unwrap();
    }
    #[tokio::test]
    async fn model_filter_postgres_default_update_reload_and_upgrade() {
        use crate::model::model_support::ModelFilterMode;
        let Some(config) = test_config() else {
            return;
        };
        let store = PostgresStore::connect_test(&config).await.unwrap();
        let request: CreateAccountRequest = serde_json::from_value(serde_json::json!({"name":"model-filter", "baseUrl":"https://example.test", "apiKey":"test-only"})).unwrap();
        let original = store.create_account_unmasked(request).await.unwrap();
        assert_eq!(original.model_filter_mode, ModelFilterMode::Allowlist);
        let changed = store
            .update_account_unmasked(
                original.id,
                serde_json::from_value(
                    serde_json::json!({"modelFilterMode":"denylist","supportedModels":["fable-5"]}),
                )
                .unwrap(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(changed.model_filter_mode, ModelFilterMode::Denylist);
        let unchanged = store
            .update_account_unmasked(original.id, UpdateAccountRequest::default())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(unchanged.model_filter_mode, ModelFilterMode::Denylist);
        assert_eq!(unchanged.supported_models, ["fable-5"]);
        assert_eq!(
            store
                .get_account(original.id, false)
                .await
                .unwrap()
                .unwrap()
                .model_filter_mode,
            ModelFilterMode::Denylist
        );
        assert!(
            store
                .list_account_eligibility()
                .await
                .unwrap()
                .iter()
                .any(|pool| pool.id == original.id
                    && pool.model_filter_mode == ModelFilterMode::Denylist)
        );
        sqlx::query("ALTER TABLE accounts DROP COLUMN model_filter_mode")
            .execute(&store.pool)
            .await
            .unwrap();
        assert!(
            store
                .verify_required_schema_compatibility(false)
                .await
                .unwrap_err()
                .to_string()
                .contains("model_filter_mode")
        );
        store.migrate_with_options(false).await.unwrap();
        assert_eq!(
            store
                .get_account(original.id, false)
                .await
                .unwrap()
                .unwrap()
                .model_filter_mode,
            ModelFilterMode::Allowlist
        );
        store.drop_test_schema().await.unwrap();
    }
}
