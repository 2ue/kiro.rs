use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Timelike, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{
    Connection, PgPool, Postgres, QueryBuilder, Row, Transaction,
    postgres::{PgPoolOptions, PgRow},
};

use crate::anthropic::model_capabilities::{
    MANUAL_SOURCE, ModelCapabilitiesStatus, ModelCapabilityItem,
};
use crate::anthropic::pricing::{
    MANUAL_PRICING_SOURCE, ModelPriceItem, ModelPricing, PricingStatus,
};
use crate::anthropic::usage::{
    CredentialCostSummary, REALTIME_USAGE_WINDOW_SECS, UsageAggregate, UsageBreakdownItem,
    UsageDashboardResponse, UsageDashboardSeries, UsageDashboardSummary, UsageDashboardTop,
    UsageDashboardWindow, UsageDashboardWindowSpec, UsageExternalPoolBillingByPool,
    UsageExternalPoolBillingSummary, UsageRealtimeStats, UsageRecord, UsageRecordQuery,
    UsageRecordStatus, UsageRecordsPageResult, UsageRecordsResult, UsageRouteKind,
    UsageSeriesPoint, UsageSource, UsageSummary, UsageTopAggregate, usage_dashboard_daily_windows,
    usage_dashboard_hourly_windows, usage_dashboard_timezone, usage_dashboard_windows,
};
use crate::external_pool::{
    CreateExternalPoolRequest, ExternalPool, ExternalPoolAuthType, ExternalPoolAutoDisablePolicy,
    ExternalPoolUsageProjectionMode, UpdateExternalPoolRequest, mask_external_pool_key,
};
use crate::kiro::model::credentials::KiroCredentials;
use crate::model::config::Config;

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
    if let sqlx::Error::Database(db_err) = &err {
        if db_err.is_unique_violation() {
            let constraint = db_err.constraint().unwrap_or_default();
            if constraint.contains("api_key") {
                return anyhow::anyhow!("凭据已存在（kiroApiKey 重复）");
            }
            if constraint.contains("refresh_token") {
                return anyhow::anyhow!("凭据已存在（refreshToken 重复）");
            }
            return anyhow::anyhow!("凭据已存在（唯一约束冲突）");
        }
    }
    anyhow::Error::new(err)
}

fn credential_from_row(row: PgRow) -> anyhow::Result<KiroCredentials> {
    let id: i64 = row.try_get("id")?;
    let priority: i32 = row.try_get("priority")?;
    let disabled: bool = row.try_get("disabled")?;
    let created_at: DateTime<Utc> = row.try_get("created_at")?;
    let updated_at: DateTime<Utc> = row.try_get("updated_at")?;
    let value: serde_json::Value = row.try_get("data")?;
    let mut credential: KiroCredentials = serde_json::from_value(value)?;
    credential.id = Some(id as u64);
    credential.created_at = Some(created_at.to_rfc3339());
    credential.updated_at = Some(updated_at.to_rfc3339());
    credential.priority = priority.max(0) as u32;
    credential.disabled = disabled;
    credential.canonicalize_auth_method();
    Ok(credential)
}

#[derive(Debug, Clone)]
pub struct PostgresStore {
    pool: PgPool,
    #[cfg(test)]
    test_schema: Option<String>,
}

impl PostgresStore {
    pub async fn connect(config: &Config) -> anyhow::Result<Self> {
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
            store
                .migrate_with_options(config.postgres.compress_usage_rollups_on_start)
                .await?;
        }
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
            .max_connections(2)
            .after_connect(move |conn, _meta| {
                let schema = schema_for_connect.clone();
                Box::pin(async move {
                    sqlx::query(&format!(r#"SET search_path TO "{}", public"#, schema))
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
        Ok(store)
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn ping(&self) -> anyhow::Result<()> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    #[cfg(test)]
    pub async fn drop_test_schema(&self) -> anyhow::Result<()> {
        let Some(schema) = &self.test_schema else {
            return Ok(());
        };
        sqlx::query(&format!(r#"DROP SCHEMA IF EXISTS "{}" CASCADE"#, schema))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn migrate_with_options(&self, compress_usage_rollups: bool) -> anyhow::Result<()> {
        const MIGRATION_LOCK_ID: i64 = 4_950_531_234_001;
        let mut conn = self.pool.acquire().await?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(MIGRATION_LOCK_ID)
            .execute(&mut *conn)
            .await?;

        let migration_result = async {
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
            if compress_usage_rollups {
                run_versioned_migration(
                    &mut conn,
                    "usage-rollup-hour-bucket-compression-v1",
                    USAGE_ROLLUP_HOUR_BUCKET_COMPRESSION_SQL,
                )
                .await?;
            }

            sqlx::query(
                r#"
                INSERT INTO schema_migrations (version, checksum, applied_at)
                VALUES ('inline-schema', $1, now())
                ON CONFLICT (version) DO UPDATE
                SET checksum = EXCLUDED.checksum,
                    applied_at = now()
                "#,
            )
            .bind(sha256_hex(SCHEMA_SQL))
            .execute(&mut *conn)
            .await?;
            Ok::<(), anyhow::Error>(())
        }
        .await;

        let unlock_result = sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(MIGRATION_LOCK_ID)
            .execute(&mut *conn)
            .await;
        migration_result?;
        unlock_result?;
        Ok(())
    }

    #[cfg(test)]
    pub async fn compress_usage_rollups_to_hour_buckets(&self) -> anyhow::Result<()> {
        self.migrate_with_options(true).await
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
        if self.load_runtime_config().await?.is_none() {
            self.save_runtime_config(file_config).await?;
        }
        Ok(())
    }

    pub async fn list_external_pools(
        &self,
        mask_secrets: bool,
    ) -> anyhow::Result<Vec<ExternalPool>> {
        let rows = sqlx::query(
            r#"
            SELECT id, name, base_url, api_key, auth_type, enabled, priority,
                   max_concurrent_requests, usage_projection_mode, auto_disable_policy,
                   auto_disabled, auto_disabled_reason, auto_disabled_at,
                   auto_disabled_until, auto_disabled_last_error, preserve_path, notes,
                   created_at, updated_at
            FROM external_upstream_pools
            WHERE deleted_at IS NULL
            ORDER BY priority ASC, id ASC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| external_pool_from_row(row, mask_secrets))
            .collect()
    }

    pub async fn get_external_pool(
        &self,
        id: u64,
        mask_secrets: bool,
    ) -> anyhow::Result<Option<ExternalPool>> {
        let row = sqlx::query(
            r#"
            SELECT id, name, base_url, api_key, auth_type, enabled, priority,
                   max_concurrent_requests, usage_projection_mode, auto_disable_policy,
                   auto_disabled, auto_disabled_reason, auto_disabled_at,
                   auto_disabled_until, auto_disabled_last_error, preserve_path, notes,
                   created_at, updated_at
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

    pub async fn create_external_pool(
        &self,
        request: CreateExternalPoolRequest,
    ) -> anyhow::Result<ExternalPool> {
        validate_external_pool_input(
            &request.name,
            &request.base_url,
            request.max_concurrent_requests,
        )?;
        let row = sqlx::query(
            r#"
            INSERT INTO external_upstream_pools (
                name, base_url, api_key, auth_type, enabled, priority,
                max_concurrent_requests, usage_projection_mode, auto_disable_policy,
                preserve_path, notes, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, now())
            RETURNING id, name, base_url, api_key, auth_type, enabled, priority,
                      max_concurrent_requests, usage_projection_mode, auto_disable_policy,
                      auto_disabled, auto_disabled_reason, auto_disabled_at,
                      auto_disabled_until, auto_disabled_last_error, preserve_path, notes,
                      created_at, updated_at
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
        .bind(request.auto_disable_policy.as_str())
        .bind(request.preserve_path)
        .bind(request.notes.map(|notes| notes.trim().to_string()))
        .fetch_one(&self.pool)
        .await?;
        external_pool_from_row(row, true)
    }

    pub async fn update_external_pool(
        &self,
        id: u64,
        request: UpdateExternalPoolRequest,
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
        let auto_disable_policy = request
            .auto_disable_policy
            .unwrap_or(current.auto_disable_policy);
        let preserve_path = request.preserve_path.unwrap_or(current.preserve_path);
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
                auto_disable_policy = $10,
                preserve_path = $11,
                notes = $12,
                updated_at = now()
            WHERE id = $1 AND deleted_at IS NULL
            RETURNING id, name, base_url, api_key, auth_type, enabled, priority,
                      max_concurrent_requests, usage_projection_mode, auto_disable_policy,
                      auto_disabled, auto_disabled_reason, auto_disabled_at,
                      auto_disabled_until, auto_disabled_last_error, preserve_path, notes,
                      created_at, updated_at
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
        .bind(auto_disable_policy.as_str())
        .bind(preserve_path)
        .bind(notes)
        .fetch_one(&self.pool)
        .await?;
        Ok(Some(external_pool_from_row(row, true)?))
    }

    pub async fn set_external_pool_enabled(
        &self,
        id: u64,
        enabled: bool,
    ) -> anyhow::Result<Option<ExternalPool>> {
        let row = sqlx::query(
            r#"
            UPDATE external_upstream_pools
            SET enabled = $2, updated_at = now()
            WHERE id = $1 AND deleted_at IS NULL
            RETURNING id, name, base_url, api_key, auth_type, enabled, priority,
                      max_concurrent_requests, usage_projection_mode, auto_disable_policy,
                      auto_disabled, auto_disabled_reason, auto_disabled_at,
                      auto_disabled_until, auto_disabled_last_error, preserve_path, notes,
                      created_at, updated_at
            "#,
        )
        .bind(id as i64)
        .bind(enabled)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| external_pool_from_row(row, true)).transpose()
    }

    pub async fn soft_delete_external_pool(&self, id: u64) -> anyhow::Result<bool> {
        let result = sqlx::query(
            "UPDATE external_upstream_pools SET deleted_at = now(), updated_at = now() WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(id as i64)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn clear_external_pool_auto_disabled(
        &self,
        id: u64,
    ) -> anyhow::Result<Option<ExternalPool>> {
        let row = sqlx::query(
            r#"
            UPDATE external_upstream_pools
            SET auto_disabled = false,
                auto_disabled_reason = NULL,
                auto_disabled_at = NULL,
                auto_disabled_until = NULL,
                auto_disabled_last_error = NULL,
                updated_at = now()
            WHERE id = $1 AND deleted_at IS NULL
            RETURNING id, name, base_url, api_key, auth_type, enabled, priority,
                      max_concurrent_requests, usage_projection_mode, auto_disable_policy,
                      auto_disabled, auto_disabled_reason, auto_disabled_at,
                      auto_disabled_until, auto_disabled_last_error, preserve_path, notes,
                      created_at, updated_at
            "#,
        )
        .bind(id as i64)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| external_pool_from_row(row, true)).transpose()
    }

    pub async fn auto_disable_external_pool(
        &self,
        id: u64,
        reason: &str,
        last_error: &str,
        duration_secs: u64,
    ) -> anyhow::Result<()> {
        let until = if duration_secs == 0 {
            None
        } else {
            Some(Utc::now() + chrono::Duration::seconds(duration_secs as i64))
        };
        sqlx::query(
            r#"
            UPDATE external_upstream_pools
            SET auto_disabled = true,
                auto_disabled_reason = $2,
                auto_disabled_at = now(),
                auto_disabled_until = $3,
                auto_disabled_last_error = $4,
                updated_at = now()
            WHERE id = $1 AND deleted_at IS NULL
            "#,
        )
        .bind(id as i64)
        .bind(reason)
        .bind(until)
        .bind(last_error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn load_credentials(&self) -> anyhow::Result<Vec<KiroCredentials>> {
        let rows = sqlx::query(
            r#"
            SELECT id, priority, disabled, data, created_at, updated_at
            FROM credentials
            WHERE deleted_at IS NULL
            ORDER BY priority ASC, id ASC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(credential_from_row).collect()
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
            SELECT id, priority, disabled, data, created_at, updated_at
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
        let api_key = api_key.trim();
        if api_key.is_empty() {
            anyhow::bail!("KIRO_API_KEY 为空");
        }
        if let Some(existing) = self.find_existing_api_key_credential(api_key).await? {
            return Ok(existing);
        }

        let credential = KiroCredentials {
            kiro_api_key: Some(api_key.to_string()),
            auth_method: Some("api_key".to_string()),
            priority: 0,
            ..Default::default()
        };
        match self.insert_credential(&credential).await {
            Ok(inserted) => Ok(inserted),
            Err(err) if err.to_string().contains("kiroApiKey 重复") => self
                .find_existing_api_key_credential(api_key)
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
        self.sync_credential_id_sequence().await?;
        Ok(())
    }

    async fn sync_credential_id_sequence(&self) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            SELECT setval(
                'credentials_id_seq',
                GREATEST(COALESCE((SELECT MAX(id) FROM credentials), 0) + 1, 1),
                false
            )
            "#,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn next_credential_id(&self) -> anyhow::Result<u64> {
        let id: i64 = sqlx::query_scalar("SELECT nextval('credentials_id_seq')")
            .fetch_one(&self.pool)
            .await?;
        Ok(id as u64)
    }

    /// 非破坏性保存凭据列表。
    ///
    /// 该方法用于首次 bootstrap 或补全旧凭据字段，只 upsert 传入行，不删除
    /// PgSQL 中其他未软删除凭据，避免旧进程内存快照覆盖其他实例新增的凭据。
    pub async fn save_credentials(&self, credentials: &[KiroCredentials]) -> anyhow::Result<()> {
        for credential in credentials {
            if credential.id.is_some() {
                self.upsert_credential(credential).await?;
            } else {
                self.insert_credential(credential).await?;
            }
        }
        Ok(())
    }

    pub async fn insert_credential(
        &self,
        credential: &KiroCredentials,
    ) -> anyhow::Result<KiroCredentials> {
        let id = match credential.id {
            Some(id) => id,
            None => self.next_credential_id().await?,
        };
        let mut canonical = credential.clone();
        canonical.id = Some(id);
        canonical.canonicalize_auth_method();
        self.upsert_credential(&canonical).await
    }

    pub async fn upsert_credential(
        &self,
        credential: &KiroCredentials,
    ) -> anyhow::Result<KiroCredentials> {
        let id = credential
            .id
            .ok_or_else(|| anyhow::anyhow!("保存到 PgSQL 的凭据必须先分配 id"))?;
        let mut canonical = credential.clone();
        canonical.id = Some(id);
        canonical.canonicalize_auth_method();
        let (auth_kind, api_key_hash, refresh_token_hash) = credential_hash_columns(&canonical);
        let value = serde_json::to_value(&canonical)?;
        let row = sqlx::query(
            r#"
            INSERT INTO credentials (
                id, priority, disabled, auth_kind, api_key_hash, refresh_token_hash,
                data, updated_at, deleted_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, now(), NULL)
            ON CONFLICT (id) DO UPDATE
            SET priority = EXCLUDED.priority,
                disabled = EXCLUDED.disabled,
                auth_kind = EXCLUDED.auth_kind,
                api_key_hash = EXCLUDED.api_key_hash,
                refresh_token_hash = EXCLUDED.refresh_token_hash,
                data = EXCLUDED.data,
                updated_at = now(),
                deleted_at = NULL
            RETURNING id, priority, disabled, data, created_at, updated_at
            "#,
        )
        .bind(id as i64)
        .bind(canonical.priority as i32)
        .bind(canonical.disabled)
        .bind(auth_kind)
        .bind(api_key_hash)
        .bind(refresh_token_hash)
        .bind(value)
        .fetch_one(&self.pool)
        .await
        .map_err(duplicate_credential_message)?;
        credential_from_row(row)
    }

    pub async fn soft_delete_credential(&self, credential_id: u64) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            UPDATE credentials
            SET deleted_at = now(),
                updated_at = now()
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
        let rows = sqlx::query(
            r#"
            SELECT credential_id, failure_count, refresh_failure_count, disabled_reason, warmup_remaining
            FROM credential_runtime_state
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        let mut states = HashMap::with_capacity(rows.len());
        for row in rows {
            let credential_id: i64 = row.try_get("credential_id")?;
            let failure_count: i32 = row.try_get("failure_count")?;
            let refresh_failure_count: i32 = row.try_get("refresh_failure_count")?;
            let disabled_reason: Option<String> = row.try_get("disabled_reason")?;
            let warmup_remaining: i32 = row.try_get("warmup_remaining")?;
            states.insert(
                credential_id as u64,
                CredentialRuntimeStateRow {
                    failure_count: failure_count.max(0) as u32,
                    refresh_failure_count: refresh_failure_count.max(0) as u32,
                    disabled_reason,
                    warmup_remaining: warmup_remaining.max(0) as u32,
                },
            );
        }
        Ok(states)
    }

    pub async fn load_credential_account_info(
        &self,
    ) -> anyhow::Result<HashMap<u64, CredentialAccountInfoRow>> {
        let rows = sqlx::query(
            r#"
            SELECT credential_id, subscription_title, current_usage, usage_limit,
                   remaining, usage_percentage, next_reset_at, checked_at
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
                remaining, usage_percentage, next_reset_at, checked_at, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now())
            ON CONFLICT (credential_id) DO UPDATE
            SET subscription_title = EXCLUDED.subscription_title,
                current_usage = EXCLUDED.current_usage,
                usage_limit = EXCLUDED.usage_limit,
                remaining = EXCLUDED.remaining,
                usage_percentage = EXCLUDED.usage_percentage,
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
    ) -> anyhow::Result<()> {
        for (credential_id, state) in states {
            self.save_credential_runtime_state_for(*credential_id, state)
                .await?;
        }
        Ok(())
    }

    pub async fn save_credential_runtime_state_for(
        &self,
        credential_id: u64,
        state: &CredentialRuntimeStateRow,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO credential_runtime_state (
                credential_id, failure_count, refresh_failure_count,
                disabled_reason, warmup_remaining, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, now())
            ON CONFLICT (credential_id) DO UPDATE
            SET failure_count = EXCLUDED.failure_count,
                refresh_failure_count = EXCLUDED.refresh_failure_count,
                disabled_reason = EXCLUDED.disabled_reason,
                warmup_remaining = EXCLUDED.warmup_remaining,
                updated_at = now()
            "#,
        )
        .bind(credential_id as i64)
        .bind(state.failure_count as i32)
        .bind(state.refresh_failure_count as i32)
        .bind(&state.disabled_reason)
        .bind(state.warmup_remaining as i32)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn save_credential_stats(
        &self,
        stats: &HashMap<u64, CredentialStatsRow>,
    ) -> anyhow::Result<()> {
        for (credential_id, stat) in stats {
            self.save_credential_stats_for(*credential_id, stat).await?;
        }
        Ok(())
    }

    pub async fn save_credential_stats_for(
        &self,
        credential_id: u64,
        stat: &CredentialStatsRow,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO credential_stats (
                credential_id, success_count, selection_count, last_used_at, updated_at
            )
            VALUES ($1, $2, $3, $4, now())
            ON CONFLICT (credential_id) DO UPDATE
            SET success_count = EXCLUDED.success_count,
                selection_count = GREATEST(credential_stats.selection_count, EXCLUDED.selection_count),
                last_used_at = EXCLUDED.last_used_at,
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

    pub async fn record_credential_success(
        &self,
        credential_id: u64,
        last_used_at: &str,
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"
            INSERT INTO credential_stats (credential_id, success_count, last_used_at, updated_at)
            VALUES ($1, 1, $2, now())
            ON CONFLICT (credential_id) DO UPDATE
            SET success_count = credential_stats.success_count + 1,
                last_used_at = EXCLUDED.last_used_at,
                updated_at = now()
            "#,
        )
        .bind(credential_id as i64)
        .bind(last_used_at)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO credential_runtime_state (
                credential_id, failure_count, refresh_failure_count,
                disabled_reason, warmup_remaining, updated_at
            )
            VALUES ($1, 0, 0, NULL, 0, now())
            ON CONFLICT (credential_id) DO UPDATE
            SET failure_count = 0,
                refresh_failure_count = 0,
                disabled_reason = NULL,
                warmup_remaining = GREATEST(credential_runtime_state.warmup_remaining - 1, 0),
                updated_at = now()
            "#,
        )
        .bind(credential_id as i64)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

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

    pub async fn record_credential_api_failure(
        &self,
        credential_id: u64,
        last_used_at: &str,
        max_failures: u32,
    ) -> anyhow::Result<CredentialRuntimeStateRow> {
        let mut tx = self.pool.begin().await?;
        upsert_last_used_at(&mut tx, credential_id, last_used_at).await?;
        let row = sqlx::query(
            r#"
            INSERT INTO credential_runtime_state (
                credential_id, failure_count, refresh_failure_count,
                disabled_reason, warmup_remaining, updated_at
            )
            VALUES ($1, 1, 0, NULL, 0, now())
            ON CONFLICT (credential_id) DO UPDATE
            SET failure_count = credential_runtime_state.failure_count + 1,
                updated_at = now()
            RETURNING failure_count, refresh_failure_count, disabled_reason, warmup_remaining
            "#,
        )
        .bind(credential_id as i64)
        .fetch_one(&mut *tx)
        .await?;
        let mut state = runtime_state_from_row(&row)?;
        if state.failure_count >= max_failures {
            state.disabled_reason = Some("TooManyFailures".to_string());
            persist_credential_disabled_in_tx(
                &mut tx,
                credential_id,
                "TooManyFailures",
                Some(state.failure_count),
                None,
            )
            .await?;
        }
        tx.commit().await?;
        Ok(state)
    }

    pub async fn record_credential_refresh_failure(
        &self,
        credential_id: u64,
        last_used_at: &str,
        max_failures: u32,
    ) -> anyhow::Result<CredentialRuntimeStateRow> {
        let mut tx = self.pool.begin().await?;
        upsert_last_used_at(&mut tx, credential_id, last_used_at).await?;
        let row = sqlx::query(
            r#"
            INSERT INTO credential_runtime_state (
                credential_id, failure_count, refresh_failure_count,
                disabled_reason, warmup_remaining, updated_at
            )
            VALUES ($1, 0, 1, NULL, 0, now())
            ON CONFLICT (credential_id) DO UPDATE
            SET refresh_failure_count = credential_runtime_state.refresh_failure_count + 1,
                updated_at = now()
            RETURNING failure_count, refresh_failure_count, disabled_reason, warmup_remaining
            "#,
        )
        .bind(credential_id as i64)
        .fetch_one(&mut *tx)
        .await?;
        let mut state = runtime_state_from_row(&row)?;
        if state.refresh_failure_count >= max_failures {
            state.disabled_reason = Some("TooManyRefreshFailures".to_string());
            persist_credential_disabled_in_tx(
                &mut tx,
                credential_id,
                "TooManyRefreshFailures",
                None,
                Some(state.refresh_failure_count),
            )
            .await?;
        }
        tx.commit().await?;
        Ok(state)
    }

    pub async fn mark_credential_disabled(
        &self,
        credential_id: u64,
        reason: &str,
        failure_count: Option<u32>,
        refresh_failure_count: Option<u32>,
        last_used_at: &str,
    ) -> anyhow::Result<CredentialRuntimeStateRow> {
        let mut tx = self.pool.begin().await?;
        upsert_last_used_at(&mut tx, credential_id, last_used_at).await?;
        let state = persist_credential_disabled_in_tx(
            &mut tx,
            credential_id,
            reason,
            failure_count,
            refresh_failure_count,
        )
        .await?;
        tx.commit().await?;
        Ok(state)
    }

    pub async fn update_credential_last_used_at(
        &self,
        credential_id: u64,
        last_used_at: &str,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO credential_stats (credential_id, success_count, last_used_at, updated_at)
            VALUES ($1, 0, $2, now())
            ON CONFLICT (credential_id) DO UPDATE
            SET last_used_at = EXCLUDED.last_used_at,
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
                id, source, last_synced_at, last_error, model_count, updated_at
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
            SELECT source, last_synced_at, last_error
            FROM model_capabilities_sync_status
            WHERE id = 'default'
            "#,
        )
        .fetch_optional(&self.pool)
        .await?;
        let (source, last_synced_at, last_error): (String, Option<String>, Option<String>) =
            if let Some(row) = status_row {
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

async fn run_versioned_migration(
    conn: &mut sqlx::pool::PoolConnection<Postgres>,
    version: &str,
    sql: &str,
) -> anyhow::Result<()> {
    let checksum = sha256_hex(sql);
    let applied_checksum: Option<String> =
        sqlx::query_scalar("SELECT checksum FROM schema_migrations WHERE version = $1")
            .bind(version)
            .fetch_optional(&mut **conn)
            .await?;
    if let Some(applied_checksum) = applied_checksum {
        if applied_checksum != checksum {
            anyhow::bail!("schema migration {version} checksum mismatch");
        }
        return Ok(());
    }

    let mut tx = conn.begin().await?;
    execute_sql_statements_in_tx(&mut tx, sql).await?;
    sqlx::query(
        r#"
        INSERT INTO schema_migrations (version, checksum, applied_at)
        VALUES ($1, $2, now())
        "#,
    )
    .bind(version)
    .bind(checksum)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
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

#[derive(Debug, Clone, Default)]
pub struct CredentialStatsRow {
    pub success_count: u64,
    pub selection_count: u64,
    pub last_used_at: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct CredentialRuntimeStateRow {
    pub failure_count: u32,
    pub refresh_failure_count: u32,
    pub disabled_reason: Option<String>,
    pub warmup_remaining: u32,
}

#[derive(Debug, Clone, Default)]
pub struct CredentialAccountInfoRow {
    pub subscription_title: Option<String>,
    pub current_usage: f64,
    pub usage_limit: f64,
    pub remaining: f64,
    pub usage_percentage: f64,
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

impl PostgresUsageStore {
    pub fn new(store: Arc<PostgresStore>) -> Self {
        Self { store }
    }

    pub async fn record(&self, record: UsageRecord) -> anyhow::Result<()> {
        let value = serde_json::to_value(&record)?;
        let created_at = chrono::DateTime::parse_from_rfc3339(&record.created_at)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
        let mut tx = self.store.pool().begin().await?;
        let old_record = sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT data FROM usage_records WHERE id = $1",
        )
        .bind(&record.id)
        .fetch_optional(&mut *tx)
        .await?
        .map(serde_json::from_value::<UsageRecord>)
        .transpose()?;
        sqlx::query(
            r#"
            INSERT INTO usage_records (
                id, created_at, endpoint, stream, model, conversation_id, credential_id,
                credential_label, status, usage_source, total_input_tokens, compat_input_tokens,
                billable_input_tokens, output_tokens, cache_read_input_tokens,
                cache_creation_input_tokens, cache_creation_5m_input_tokens,
                cache_creation_1h_input_tokens, estimated_cost_usd, pricing_available,
                pricing_model, duration_ms, simulated, sticky_bound, fallback_from_sticky,
                error_type, error_message, error_detail, data
            )
            VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                $11, $12, $13, $14, $15, $16, $17, $18,
                $19, $20, $21, $22, $23, $24, $25, $26, $27, $28, $29
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
                deleted_at = NULL,
                updated_at = now()
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
        .bind(record.pricing_available)
        .bind(&record.pricing_model)
        .bind(record.duration_ms as i64)
        .bind(record.simulated)
        .bind(record.sticky_bound)
        .bind(record.fallback_from_sticky)
        .bind(&record.error_type)
        .bind(&record.error_message)
        .bind(&record.error_detail)
        .bind(value)
        .execute(&mut *tx)
        .await?;
        if let Some(old_record) = old_record {
            apply_usage_rollup_delta(&mut tx, &old_record, -1).await?;
        }
        apply_usage_rollup_delta(&mut tx, &record, 1).await?;
        tx.commit().await?;
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

    pub async fn clear(&self) -> anyhow::Result<()> {
        sqlx::query("UPDATE usage_records SET deleted_at = now(), updated_at = now() WHERE deleted_at IS NULL")
            .execute(self.store.pool())
            .await?;
        Ok(())
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

    pub async fn soft_delete_cleanup_batch(
        &self,
        cutoff: DateTime<Utc>,
        batch_size: usize,
    ) -> anyhow::Result<u64> {
        let result = sqlx::query(
            r#"
            WITH victims AS (
                SELECT id
                FROM usage_records
                WHERE deleted_at IS NULL
                  AND created_at < $1
                ORDER BY created_at ASC, id ASC
                LIMIT $2
            )
            UPDATE usage_records AS u
            SET deleted_at = now(), updated_at = now()
            FROM victims AS v
            WHERE u.id = v.id
            "#,
        )
        .bind(cutoff)
        .bind(usize_to_i64(batch_size))
        .execute(self.store.pool())
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn hard_delete_cleanup_batch(
        &self,
        cutoff: DateTime<Utc>,
        batch_size: usize,
    ) -> anyhow::Result<u64> {
        let result = sqlx::query(
            r#"
            WITH victims AS (
                SELECT id
                FROM usage_records
                WHERE deleted_at IS NOT NULL
                  AND deleted_at < $1
                ORDER BY deleted_at ASC, id ASC
                LIMIT $2
            )
            DELETE FROM usage_records AS u
            USING victims AS v
            WHERE u.id = v.id
            "#,
        )
        .bind(cutoff)
        .bind(usize_to_i64(batch_size))
        .execute(self.store.pool())
        .await?;
        Ok(result.rows_affected())
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
                COALESCE(t.external_pool_shaped_cost_usd, 0)::double precision AS external_pool_shaped_cost_usd,
                COALESCE(t.external_pool_uplifted_cost_usd, 0)::double precision AS external_pool_uplifted_cost_usd,
                COALESCE(t.external_pool_profit_usd, 0)::double precision AS external_pool_profit_usd,
                COALESCE(t.external_pool_reported_cost_usd, 0)::double precision AS external_pool_reported_cost_usd,
                COALESCE(t.external_pool_billable_cost_usd, 0)::double precision AS external_pool_billable_cost_usd,
                COALESCE(t.external_pool_cost_floor_delta_usd, 0)::double precision AS external_pool_cost_floor_delta_usd,
                COUNT(r.id) FILTER (WHERE r.created_at >= now() - interval '60 seconds')::bigint AS realtime_requests,
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
                     t.total_estimated_cost_usd, t.priced_requests, t.unpriced_requests,
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
            realtime: UsageRealtimeStats::from_totals(
                REALTIME_USAGE_WINDOW_SECS,
                row_i64_to_usize(&row, "realtime_requests")?,
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
        let (timezone, offset) = usage_dashboard_timezone(timezone);
        let window_specs = usage_dashboard_windows(now, offset);
        let mut windows = self
            .dashboard_windows(&window_specs, high_cache_threshold)
            .await?;
        let window_totals: HashMap<String, usize> = windows
            .iter()
            .map(|window| (window.key.clone(), window.summary.total_requests))
            .collect();
        let mut status_breakdown = self
            .dashboard_breakdown(
                &window_specs,
                &window_totals,
                DashboardBreakdownColumn::Status,
            )
            .await?;
        let mut usage_source_breakdown = self
            .dashboard_breakdown(
                &window_specs,
                &window_totals,
                DashboardBreakdownColumn::UsageSource,
            )
            .await?;
        let mut external_pool_billing_by_pool = self
            .dashboard_external_pool_billing_by_pool(&window_specs)
            .await?;

        for window in &mut windows {
            window.summary.status_breakdown =
                status_breakdown.remove(&window.key).unwrap_or_default();
            window.summary.usage_source_breakdown = usage_source_breakdown
                .remove(&window.key)
                .unwrap_or_default();
            window.summary.external_pool_billing_by_pool = external_pool_billing_by_pool
                .remove(&window.key)
                .unwrap_or_default();
        }

        let hourly_specs = usage_dashboard_hourly_windows(now, offset);
        let daily_specs = usage_dashboard_daily_windows(now, offset);
        Ok(UsageDashboardResponse {
            generated_at: now.to_rfc3339(),
            timezone,
            windows,
            series: UsageDashboardSeries {
                hourly_24h: self.dashboard_series(&hourly_specs).await?,
                daily_7d: self.dashboard_series(&daily_specs).await?,
            },
            top: UsageDashboardTop {
                window_key: "lifetime".to_string(),
                models: self
                    .dashboard_top_aggregates(DashboardTopGroup::Model)
                    .await?,
                credentials: self
                    .dashboard_top_aggregates(DashboardTopGroup::Credential)
                    .await?,
                endpoints: self
                    .dashboard_top_aggregates(DashboardTopGroup::Endpoint)
                    .await?,
                errors: self
                    .dashboard_top_aggregates(DashboardTopGroup::Error)
                    .await?,
            },
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
        builder.push(
            r#"
            , rollup AS (
            SELECT
                w.key,
                w.label,
                w.from_at,
                w.to_at,
                COALESCE(SUM(b.requests), 0)::bigint AS total_requests,
                COALESCE(SUM(b.success_requests), 0)::bigint AS success_requests,
                COALESCE(SUM(b.error_requests), 0)::bigint AS error_requests,
                COALESCE(SUM(b.stream_requests), 0)::bigint AS stream_requests,
                COALESCE(SUM(b.non_stream_requests), 0)::bigint AS non_stream_requests,
                COALESCE(SUM(b.total_input_tokens), 0)::bigint AS total_input_tokens,
                COALESCE(SUM(b.billable_input_tokens), 0)::bigint AS billable_input_tokens,
                COALESCE(SUM(b.total_output_tokens), 0)::bigint AS total_output_tokens,
                COALESCE(SUM(b.total_cache_read_input_tokens), 0)::bigint AS total_cache_read_input_tokens,
                COALESCE(SUM(b.total_cache_creation_input_tokens), 0)::bigint AS total_cache_creation_input_tokens,
                COALESCE(SUM(b.total_estimated_cost_usd), 0)::double precision AS total_estimated_cost_usd,
                COALESCE(SUM(b.priced_requests), 0)::bigint AS priced_requests,
                COALESCE(SUM(b.unpriced_requests), 0)::bigint AS unpriced_requests,
                CASE
                    WHEN COALESCE(SUM(b.duration_ms_count), 0) > 0
                    THEN COALESCE(SUM(b.duration_ms_sum), 0)::double precision
                         / SUM(b.duration_ms_count)::double precision
                    ELSE 0
                END AS average_duration_ms,
                COALESCE(MAX(b.duration_ms_max), 0)::bigint AS p95_duration_ms,
                COALESCE(SUM(b.sticky_bound_requests), 0)::bigint AS sticky_bound_requests,
                COALESCE(SUM(b.fallback_from_sticky_requests), 0)::bigint AS fallback_from_sticky_requests,
                COALESCE(SUM(b.simulated_requests), 0)::bigint AS simulated_requests,
                COALESCE(SUM(b.upstream_metadata_requests), 0)::bigint AS upstream_metadata_requests,
                COALESCE(SUM(b.external_pool_requests), 0)::bigint AS external_pool_requests,
                COALESCE(SUM(b.external_pool_priced_requests), 0)::bigint AS external_pool_priced_requests,
                COALESCE(SUM(b.external_pool_unpriced_requests), 0)::bigint AS external_pool_unpriced_requests,
                COALESCE(SUM(b.external_pool_cost_floor_applied_requests), 0)::bigint AS external_pool_cost_floor_applied_requests,
                COALESCE(SUM(b.external_pool_raw_cost_usd), 0)::double precision AS external_pool_raw_cost_usd,
                COALESCE(SUM(b.external_pool_shaped_cost_usd), 0)::double precision AS external_pool_shaped_cost_usd,
                COALESCE(SUM(b.external_pool_uplifted_cost_usd), 0)::double precision AS external_pool_uplifted_cost_usd,
                COALESCE(SUM(b.external_pool_profit_usd), 0)::double precision AS external_pool_profit_usd,
                COALESCE(SUM(b.external_pool_reported_cost_usd), 0)::double precision AS external_pool_reported_cost_usd,
                COALESCE(SUM(b.external_pool_billable_cost_usd), 0)::double precision AS external_pool_billable_cost_usd,
                COALESCE(SUM(b.external_pool_cost_floor_delta_usd), 0)::double precision AS external_pool_cost_floor_delta_usd
            FROM windows w
            LEFT JOIN usage_rollup_time_buckets b
                ON b.dimension = 'global'
                AND b.dimension_key = 'all'
                AND b.bucket_start >= w.from_at
                AND b.bucket_start < w.to_at
            GROUP BY w.key, w.label, w.from_at, w.to_at, w.ord
            ), high_cache AS (
            SELECT
                w.key,
                COALESCE(SUM(c.requests), 0)::bigint AS high_cache_requests
            FROM windows w
            LEFT JOIN usage_cache_read_rollup_time_buckets c
                ON c.bucket_start >= w.from_at
                AND c.bucket_start < w.to_at
                AND c.cache_read_input_tokens >=
            "#,
        );
        builder.push_bind(high_cache_threshold);
        builder.push(
            r#"
            GROUP BY w.key
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
                r.priced_requests,
                r.unpriced_requests,
                r.average_duration_ms,
                r.p95_duration_ms,
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
            JOIN windows w ON w.key = r.key
            ORDER BY w.ord
            "#,
        );

        let rows = builder.build().fetch_all(self.store.pool()).await?;
        rows.into_iter()
            .map(dashboard_window_from_row)
            .collect::<anyhow::Result<Vec<_>>>()
    }

    async fn dashboard_breakdown(
        &self,
        specs: &[UsageDashboardWindowSpec],
        totals: &HashMap<String, usize>,
        column: DashboardBreakdownColumn,
    ) -> anyhow::Result<HashMap<String, Vec<UsageBreakdownItem>>> {
        if specs.is_empty() {
            return Ok(HashMap::new());
        }

        let item_dimension = column.rollup_dimension();
        let mut builder = QueryBuilder::<Postgres>::new("");
        push_dashboard_windows_cte(&mut builder, specs);
        builder.push(
            r#"SELECT w.key AS window_key,
                   b.dimension_key AS item_key,
                   SUM(b.requests)::bigint AS requests
            FROM windows w
            JOIN usage_rollup_time_buckets b
              ON b.dimension = "#,
        );
        builder.push_bind(item_dimension);
        builder.push(
            r#"
             AND b.bucket_start >= w.from_at
             AND b.bucket_start < w.to_at
            GROUP BY w.key, w.ord, b.dimension_key
            HAVING SUM(b.requests) > 0
            "#,
        );
        builder.push(" ORDER BY w.ord, requests DESC, item_key");

        let rows = builder.build().fetch_all(self.store.pool()).await?;
        let mut grouped: HashMap<String, Vec<UsageBreakdownItem>> = HashMap::new();
        for row in rows {
            let window_key: String = row.try_get("window_key")?;
            let item_key: String = row.try_get("item_key")?;
            let requests = row_i64_to_usize(&row, "requests")?;
            let total_requests = totals.get(&window_key).copied().unwrap_or_default();
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
        builder.push(
            r#"
            SELECT
                w.key,
                w.label,
                w.from_at,
                w.to_at,
                COALESCE(SUM(b.requests), 0)::bigint AS requests,
                COALESCE(SUM(b.success_requests), 0)::bigint AS success_requests,
                COALESCE(SUM(b.error_requests), 0)::bigint AS error_requests,
                COALESCE(SUM(b.total_input_tokens), 0)::bigint AS total_input_tokens,
                COALESCE(SUM(b.billable_input_tokens), 0)::bigint AS billable_input_tokens,
                COALESCE(SUM(b.total_output_tokens), 0)::bigint AS total_output_tokens,
                COALESCE(SUM(b.total_estimated_cost_usd), 0)::double precision AS total_estimated_cost_usd
            FROM windows w
            LEFT JOIN usage_rollup_time_buckets b
                ON b.dimension = 'global'
                AND b.dimension_key = 'all'
                AND b.bucket_start >= w.from_at
                AND b.bucket_start < w.to_at
            GROUP BY w.key, w.label, w.from_at, w.to_at, w.ord
            ORDER BY w.ord
            "#,
        );
        let rows = builder.build().fetch_all(self.store.pool()).await?;
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
        builder.push(
            r#"
            SELECT
                w.key AS window_key,
                b.dimension_key AS pool_id,
                NULLIF(MAX(b.dimension_label), '') AS pool_name,
                COALESCE(SUM(b.external_pool_requests), 0)::bigint AS requests,
                COALESCE(SUM(b.external_pool_priced_requests), 0)::bigint AS priced_requests,
                COALESCE(SUM(b.external_pool_unpriced_requests), 0)::bigint AS unpriced_requests,
                COALESCE(SUM(b.external_pool_cost_floor_applied_requests), 0)::bigint AS cost_floor_applied_requests,
                COALESCE(SUM(b.external_pool_raw_cost_usd), 0)::double precision AS raw_cost_usd,
                COALESCE(SUM(b.external_pool_shaped_cost_usd), 0)::double precision AS shaped_cost_usd,
                COALESCE(SUM(b.external_pool_uplifted_cost_usd), 0)::double precision AS uplifted_cost_usd,
                COALESCE(SUM(b.external_pool_profit_usd), 0)::double precision AS profit_usd,
                COALESCE(SUM(b.external_pool_reported_cost_usd), 0)::double precision AS reported_cost_usd,
                COALESCE(SUM(b.external_pool_billable_cost_usd), 0)::double precision AS billable_cost_usd,
                COALESCE(SUM(b.external_pool_cost_floor_delta_usd), 0)::double precision AS cost_floor_delta_usd
            FROM windows w
            JOIN usage_rollup_time_buckets b
              ON b.dimension = 'external_pool'
             AND b.bucket_start >= w.from_at
             AND b.bucket_start < w.to_at
            GROUP BY w.key, w.ord, b.dimension_key
            HAVING COALESCE(SUM(b.external_pool_requests), 0) > 0
            ORDER BY w.ord, uplifted_cost_usd DESC, requests DESC, pool_id
            "#,
        );

        let rows = builder.build().fetch_all(self.store.pool()).await?;
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
    ) -> anyhow::Result<Vec<UsageTopAggregate>> {
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
                total_estimated_cost_usd::double precision AS total_estimated_cost_usd
            FROM usage_rollup_totals
            WHERE dimension = "#,
        );
        builder.push_bind(group.rollup_dimension());
        builder.push(group.rollup_extra_where());
        builder.push(" AND requests > 0 ");
        builder.push(
            " ORDER BY total_estimated_cost_usd DESC, requests DESC, total_input_tokens DESC LIMIT 10",
        );

        let rows = builder.build().fetch_all(self.store.pool()).await?;
        rows.into_iter()
            .map(usage_top_aggregate_from_row)
            .collect::<anyhow::Result<Vec<_>>>()
    }

    pub async fn credential_cost_summary(
        &self,
    ) -> anyhow::Result<HashMap<u64, CredentialCostSummary>> {
        let rows = sqlx::query(
            r#"
            SELECT
                credential_id,
                estimated_cost_usd,
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
                total_estimated_cost_usd AS estimated_cost_usd
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
                total_estimated_cost_usd AS estimated_cost_usd
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
            records.push(serde_json::from_value(value)?);
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

fn cleanup_preview_from_row(row: PgRow) -> anyhow::Result<UsageCleanupPreview> {
    let matched_rows = row_i64_to_u64(&row, "matched_rows")?;
    Ok(UsageCleanupPreview {
        matched_rows,
        oldest_created_at: row.try_get("oldest_created_at")?,
        newest_created_at: row.try_get("newest_created_at")?,
    })
}

#[derive(Clone, Copy)]
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
}

struct UsageRollupDimension {
    dimension: &'static str,
    key: String,
    label: Option<String>,
    include_time_bucket: bool,
}

async fn apply_usage_rollup_delta(
    tx: &mut Transaction<'_, Postgres>,
    record: &UsageRecord,
    direction: i64,
) -> anyhow::Result<()> {
    let direction = if direction < 0 { -1 } else { 1 };
    let metrics = UsageRollupMetrics::from_record(record, direction);
    let created_at = parse_usage_created_at(record);
    let bucket_start = usage_rollup_bucket_start(created_at);
    let dimensions = usage_rollup_dimensions(record);

    for dimension in dimensions {
        upsert_usage_rollup_total(tx, &dimension, metrics).await?;
        if dimension.include_time_bucket {
            upsert_usage_rollup_time_bucket(tx, bucket_start, &dimension, metrics).await?;
        }
    }

    let cache_read = record.cache_read_input_tokens.max(0);
    upsert_usage_cache_read_total(tx, cache_read, direction).await?;
    upsert_usage_cache_read_time_bucket(tx, bucket_start, cache_read, direction).await?;
    upsert_usage_duration_time_bucket(
        tx,
        bucket_start,
        record.duration_ms.min(i32::MAX as u64) as i32,
        direction,
    )
    .await?;

    if let Some(credential_id) = record.credential_id {
        upsert_credential_usage_summary(tx, credential_id, record, direction).await?;
    }

    Ok(())
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
            external_pool_requests, external_pool_priced_requests,
            external_pool_unpriced_requests, external_pool_cost_floor_applied_requests,
            external_pool_raw_cost_usd, external_pool_shaped_cost_usd,
            external_pool_uplifted_cost_usd, external_pool_profit_usd,
            external_pool_reported_cost_usd, external_pool_billable_cost_usd,
            external_pool_cost_floor_delta_usd,
            duration_ms_sum, duration_ms_count, duration_ms_max, updated_at
        )
        VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
            $11, $12, $13, $14, $15, $16, $17, $18,
            $19, $20, $21, $22, $23, $24, $25, $26,
            $27, $28, $29, $30, $31, $32, $33, $34,
            $35, $36, $37, $38, now()
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
            duration_ms_max = GREATEST(usage_rollup_totals.duration_ms_max, EXCLUDED.duration_ms_max),
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
            total_estimated_cost_usd, external_pool_requests,
            external_pool_priced_requests, external_pool_unpriced_requests,
            external_pool_cost_floor_applied_requests, external_pool_raw_cost_usd,
            external_pool_shaped_cost_usd, external_pool_uplifted_cost_usd,
            external_pool_profit_usd, external_pool_reported_cost_usd,
            external_pool_billable_cost_usd, external_pool_cost_floor_delta_usd,
            duration_ms_sum, duration_ms_count,
            duration_ms_max, updated_at
        )
        VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
            $11, $12, $13, $14, $15, $16, $17, $18,
            $19, $20, $21, $22, $23, $24, $25, $26,
            $27, $28, $29, $30, $31, $32, $33, $34,
            $35, $36, $37, $38, $39, now()
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
            duration_ms_max = GREATEST(usage_rollup_time_buckets.duration_ms_max, EXCLUDED.duration_ms_max),
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

async fn upsert_credential_usage_summary(
    tx: &mut Transaction<'_, Postgres>,
    credential_id: u64,
    record: &UsageRecord,
    direction: i64,
) -> anyhow::Result<()> {
    let sign = if direction < 0 { -1 } else { 1 };
    sqlx::query(
        r#"
        INSERT INTO usage_credential_cost_summary (
            credential_id, requests, estimated_cost_usd, priced_requests,
            unpriced_requests, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, now())
        ON CONFLICT (credential_id) DO UPDATE
        SET requests = usage_credential_cost_summary.requests + EXCLUDED.requests,
            estimated_cost_usd = usage_credential_cost_summary.estimated_cost_usd + EXCLUDED.estimated_cost_usd,
            priced_requests = usage_credential_cost_summary.priced_requests + EXCLUDED.priced_requests,
            unpriced_requests = usage_credential_cost_summary.unpriced_requests + EXCLUDED.unpriced_requests,
            updated_at = now()
        "#,
    )
    .bind(credential_id as i64)
    .bind(sign)
    .bind(record.estimated_cost_usd * sign as f64)
    .bind(signed_bool(record.pricing_available, sign))
    .bind(signed_bool(!record.pricing_available, sign))
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
}

#[derive(Clone, Copy)]
enum DashboardTopGroup {
    Model,
    Credential,
    Endpoint,
    Error,
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
}

fn normalize_query_limit(limit: usize) -> usize {
    if limit == 0 { 100 } else { limit.min(1000) }
}

fn normalize_page_limit(limit: usize) -> usize {
    if limit == 0 { 20 } else { limit.min(1000) }
}

fn push_usage_filters(builder: &mut QueryBuilder<'_, Postgres>, query: &UsageRecordQuery) {
    builder.push(" WHERE deleted_at IS NULL");

    if let Some(q) = query.q.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        let pattern = format!("%{}%", q);
        builder.push(" AND (");
        let fields = [
            "id",
            "created_at::text",
            "endpoint",
            "model",
            "conversation_id",
            "credential_label",
            "status",
            "usage_source",
            "error_type",
            "error_message",
            "error_detail",
            "pricing_model",
            "credential_id::text",
            "data->>'externalPoolId'",
            "data->>'externalPoolName'",
            "estimated_cost_usd::text",
            "data::text",
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
    if let Some(conversation_id) = &query.conversation_id {
        builder.push(" AND conversation_id = ");
        builder.push_bind(conversation_id.clone());
    }
    if let Some(credential_id) = query.credential_id {
        builder.push(" AND credential_id = ");
        builder.push_bind(credential_id as i64);
    }
    if let Some(external_pool_id) = query.external_pool_id {
        builder.push(" AND data->>'externalPoolId' = ");
        builder.push_bind(external_pool_id.to_string());
    }
    if let Some(model) = &query.model {
        builder.push(" AND model = ");
        builder.push_bind(model.clone());
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
    builder.push(") ");
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

fn runtime_state_from_row(row: &PgRow) -> anyhow::Result<CredentialRuntimeStateRow> {
    let failure_count: i32 = row.try_get("failure_count")?;
    let refresh_failure_count: i32 = row.try_get("refresh_failure_count")?;
    let disabled_reason: Option<String> = row.try_get("disabled_reason")?;
    let warmup_remaining: i32 = row.try_get("warmup_remaining")?;
    Ok(CredentialRuntimeStateRow {
        failure_count: failure_count.max(0) as u32,
        refresh_failure_count: refresh_failure_count.max(0) as u32,
        disabled_reason,
        warmup_remaining: warmup_remaining.max(0) as u32,
    })
}

fn external_pool_from_row(row: PgRow, mask_secrets: bool) -> anyhow::Result<ExternalPool> {
    let id: i64 = row.try_get("id")?;
    let api_key: String = row.try_get("api_key")?;
    let auth_type: String = row.try_get("auth_type")?;
    let usage_projection_mode: String = row.try_get("usage_projection_mode")?;
    let auto_disable_policy: String = row.try_get("auto_disable_policy")?;
    let max_concurrent_requests: i32 = row.try_get("max_concurrent_requests")?;
    Ok(ExternalPool {
        id: id.max(0) as u64,
        name: row.try_get("name")?,
        base_url: row.try_get("base_url")?,
        api_key: (!mask_secrets).then_some(api_key.clone()),
        masked_api_key: Some(mask_external_pool_key(&api_key)),
        auth_type: ExternalPoolAuthType::parse(&auth_type),
        enabled: row.try_get("enabled")?,
        priority: row.try_get("priority")?,
        max_concurrent_requests: max_concurrent_requests.max(1) as u32,
        usage_projection_mode: ExternalPoolUsageProjectionMode::parse(&usage_projection_mode),
        auto_disable_policy: ExternalPoolAutoDisablePolicy::parse(&auto_disable_policy),
        auto_disabled: row.try_get("auto_disabled")?,
        auto_disabled_reason: row.try_get("auto_disabled_reason")?,
        auto_disabled_at: row.try_get("auto_disabled_at")?,
        auto_disabled_until: row.try_get("auto_disabled_until")?,
        auto_disabled_last_error: row.try_get("auto_disabled_last_error")?,
        preserve_path: row.try_get("preserve_path")?,
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

async fn upsert_last_used_at(
    tx: &mut Transaction<'_, Postgres>,
    credential_id: u64,
    last_used_at: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO credential_stats (credential_id, success_count, last_used_at, updated_at)
        VALUES ($1, 0, $2, now())
        ON CONFLICT (credential_id) DO UPDATE
        SET last_used_at = EXCLUDED.last_used_at,
            updated_at = now()
        "#,
    )
    .bind(credential_id as i64)
    .bind(last_used_at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn persist_credential_disabled_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    credential_id: u64,
    reason: &str,
    failure_count: Option<u32>,
    refresh_failure_count: Option<u32>,
) -> anyhow::Result<CredentialRuntimeStateRow> {
    let row = sqlx::query(
        r#"
        INSERT INTO credential_runtime_state (
            credential_id, failure_count, refresh_failure_count,
            disabled_reason, warmup_remaining, updated_at
        )
        VALUES ($1, COALESCE($2, 0), COALESCE($3, 0), $4, 0, now())
        ON CONFLICT (credential_id) DO UPDATE
        SET failure_count = COALESCE($2, credential_runtime_state.failure_count),
            refresh_failure_count = COALESCE($3, credential_runtime_state.refresh_failure_count),
            disabled_reason = $4,
            updated_at = now()
        RETURNING failure_count, refresh_failure_count, disabled_reason, warmup_remaining
        "#,
    )
    .bind(credential_id as i64)
    .bind(failure_count.map(|value| value as i32))
    .bind(refresh_failure_count.map(|value| value as i32))
    .bind(reason)
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query(
        r#"
        UPDATE credentials
        SET disabled = TRUE,
            data = jsonb_set(data, '{disabled}', 'true'::jsonb, true),
            updated_at = now()
        WHERE id = $1 AND deleted_at IS NULL
        "#,
    )
    .bind(credential_id as i64)
    .execute(&mut **tx)
    .await?;
    runtime_state_from_row(&row)
}

fn usize_to_i64(value: usize) -> i64 {
    value.min(i64::MAX as usize) as i64
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
    deleted_at TIMESTAMPTZ
);

SELECT setval(
    'credentials_id_seq',
    GREATEST(COALESCE((SELECT MAX(id) FROM credentials), 0) + 1, 1),
    false
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
    name TEXT NOT NULL,
    base_url TEXT NOT NULL,
    api_key TEXT NOT NULL,
    auth_type TEXT NOT NULL DEFAULT 'bearer',
    enabled BOOLEAN NOT NULL DEFAULT true,
    priority INTEGER NOT NULL DEFAULT 100,
    max_concurrent_requests INTEGER NOT NULL DEFAULT 10,
    usage_projection_mode TEXT NOT NULL DEFAULT 'pass_through',
    auto_disable_policy TEXT NOT NULL DEFAULT 'inherit',
    auto_disabled BOOLEAN NOT NULL DEFAULT false,
    auto_disabled_reason TEXT,
    auto_disabled_at TIMESTAMPTZ,
    auto_disabled_until TIMESTAMPTZ,
    auto_disabled_last_error TEXT,
    preserve_path BOOLEAN NOT NULL DEFAULT true,
    notes TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at TIMESTAMPTZ
);

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
    ADD COLUMN IF NOT EXISTS auto_disable_policy TEXT NOT NULL DEFAULT 'inherit';

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

ALTER TABLE credential_stats
    ADD COLUMN IF NOT EXISTS selection_count BIGINT NOT NULL DEFAULT 0;

CREATE TABLE IF NOT EXISTS credential_runtime_state (
    credential_id BIGINT PRIMARY KEY REFERENCES credentials(id) ON DELETE CASCADE,
    failure_count INTEGER NOT NULL DEFAULT 0,
    refresh_failure_count INTEGER NOT NULL DEFAULT 0,
    disabled_reason TEXT,
    warmup_remaining INTEGER NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS credential_account_info (
    credential_id BIGINT PRIMARY KEY REFERENCES credentials(id) ON DELETE CASCADE,
    subscription_title TEXT,
    current_usage DOUBLE PRECISION NOT NULL DEFAULT 0,
    usage_limit DOUBLE PRECISION NOT NULL DEFAULT 0,
    remaining DOUBLE PRECISION NOT NULL DEFAULT 0,
    usage_percentage DOUBLE PRECISION NOT NULL DEFAULT 0,
    next_reset_at DOUBLE PRECISION,
    checked_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

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
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE usage_records
    ADD COLUMN IF NOT EXISTS deleted_at TIMESTAMPTZ;

CREATE INDEX IF NOT EXISTS idx_usage_records_created_at ON usage_records (created_at DESC) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_usage_records_credential_created ON usage_records (credential_id, created_at DESC) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_usage_records_model_created ON usage_records (model, created_at DESC) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_usage_records_status_created ON usage_records (status, created_at DESC) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_usage_records_conversation ON usage_records (conversation_id) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_usage_records_deleted_at ON usage_records (deleted_at ASC, id ASC) WHERE deleted_at IS NOT NULL;

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

CREATE INDEX IF NOT EXISTS idx_usage_rollup_totals_dimension_cost
    ON usage_rollup_totals (dimension, total_estimated_cost_usd DESC, requests DESC);

ALTER TABLE usage_rollup_totals
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
    ADD COLUMN IF NOT EXISTS external_pool_cost_floor_delta_usd DOUBLE PRECISION NOT NULL DEFAULT 0;

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

CREATE INDEX IF NOT EXISTS idx_usage_rollup_time_dimension_bucket
    ON usage_rollup_time_buckets (dimension, bucket_start);

CREATE INDEX IF NOT EXISTS idx_usage_rollup_time_dimension_key_bucket
    ON usage_rollup_time_buckets (dimension, dimension_key, bucket_start);

ALTER TABLE usage_rollup_time_buckets
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
    ADD COLUMN IF NOT EXISTS external_pool_cost_floor_delta_usd DOUBLE PRECISION NOT NULL DEFAULT 0;

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

CREATE INDEX IF NOT EXISTS idx_usage_cache_read_rollup_time_bucket
    ON usage_cache_read_rollup_time_buckets (bucket_start, cache_read_input_tokens);

CREATE TABLE IF NOT EXISTS usage_duration_rollup_time_buckets (
    bucket_start TIMESTAMPTZ NOT NULL,
    duration_ms INTEGER NOT NULL,
    requests BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (bucket_start, duration_ms)
);

CREATE INDEX IF NOT EXISTS idx_usage_duration_rollup_time_bucket
    ON usage_duration_rollup_time_buckets (bucket_start, duration_ms);

CREATE TABLE IF NOT EXISTS usage_credential_cost_summary (
    credential_id BIGINT NOT NULL PRIMARY KEY,
    requests BIGINT NOT NULL DEFAULT 0,
    estimated_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    priced_requests BIGINT NOT NULL DEFAULT 0,
    unpriced_requests BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_usage_credential_cost_summary_cost
    ON usage_credential_cost_summary (estimated_cost_usd DESC, requests DESC);

INSERT INTO usage_rollup_totals (
    dimension, dimension_key, dimension_label, requests, success_requests, error_requests,
    stream_requests, non_stream_requests, priced_requests, unpriced_requests,
    local_prompt_cache_requests, simulated_requests, upstream_metadata_requests,
    sticky_bound_requests, fallback_from_sticky_requests, total_input_tokens,
    billable_input_tokens, total_output_tokens, total_cache_read_input_tokens,
    total_cache_creation_input_tokens, local_prompt_cache_input_tokens,
    local_prompt_cache_read_input_tokens, local_prompt_cache_creation_input_tokens,
    total_estimated_cost_usd, external_pool_requests, external_pool_priced_requests,
    external_pool_unpriced_requests, external_pool_cost_floor_applied_requests,
    external_pool_raw_cost_usd, external_pool_shaped_cost_usd,
    external_pool_uplifted_cost_usd, external_pool_profit_usd,
    external_pool_reported_cost_usd, external_pool_billable_cost_usd,
    external_pool_cost_floor_delta_usd,
    duration_ms_sum, duration_ms_count, duration_ms_max
)
SELECT
    d.dimension,
    d.dimension_key,
    MAX(d.dimension_label),
    COUNT(*)::bigint,
    COUNT(*) FILTER (WHERE r.status = 'success')::bigint,
    COUNT(*) FILTER (WHERE r.status <> 'success')::bigint,
    COUNT(*) FILTER (WHERE r.stream)::bigint,
    COUNT(*) FILTER (WHERE NOT r.stream)::bigint,
    COUNT(*) FILTER (WHERE r.pricing_available)::bigint,
    COUNT(*) FILTER (WHERE NOT r.pricing_available)::bigint,
    COUNT(*) FILTER (WHERE r.usage_source = 'local_prompt_cache')::bigint,
    COUNT(*) FILTER (WHERE r.simulated)::bigint,
    COUNT(*) FILTER (WHERE r.usage_source = 'upstream_metadata')::bigint,
    COUNT(*) FILTER (WHERE r.sticky_bound)::bigint,
    COUNT(*) FILTER (WHERE r.fallback_from_sticky)::bigint,
    COALESCE(SUM(r.total_input_tokens), 0)::bigint,
    COALESCE(SUM(r.billable_input_tokens), 0)::bigint,
    COALESCE(SUM(r.output_tokens), 0)::bigint,
    COALESCE(SUM(r.cache_read_input_tokens), 0)::bigint,
    COALESCE(SUM(r.cache_creation_input_tokens), 0)::bigint,
    COALESCE(SUM(r.total_input_tokens) FILTER (WHERE r.usage_source = 'local_prompt_cache'), 0)::bigint,
    COALESCE(SUM(r.cache_read_input_tokens) FILTER (WHERE r.usage_source = 'local_prompt_cache'), 0)::bigint,
    COALESCE(SUM(r.cache_creation_input_tokens) FILTER (WHERE r.usage_source = 'local_prompt_cache'), 0)::bigint,
    COALESCE(SUM(r.estimated_cost_usd), 0)::double precision,
    COUNT(*) FILTER (WHERE r.data->>'routeKind' = 'external_pool')::bigint,
    COUNT(*) FILTER (
        WHERE r.data->>'routeKind' = 'external_pool'
          AND r.data #>> '{externalPoolBilling,pricingAvailable}' = 'true'
    )::bigint,
    COUNT(*) FILTER (
        WHERE r.data->>'routeKind' = 'external_pool'
          AND COALESCE(r.data #>> '{externalPoolBilling,pricingAvailable}', 'false') <> 'true'
    )::bigint,
    COUNT(*) FILTER (
        WHERE r.data->>'routeKind' = 'external_pool'
          AND r.data #>> '{externalPoolBilling,costFloorApplied}' = 'true'
    )::bigint,
    COALESCE(SUM(NULLIF(r.data #>> '{externalPoolBilling,rawCostUsd}', '')::double precision), 0)::double precision,
    COALESCE(SUM(COALESCE(
        NULLIF(r.data #>> '{externalPoolBilling,shapedCostUsd}', '')::double precision,
        NULLIF(r.data #>> '{externalPoolBilling,reportedCostUsd}', '')::double precision,
        0
    )), 0)::double precision,
    COALESCE(SUM(COALESCE(
        NULLIF(r.data #>> '{externalPoolBilling,upliftedCostUsd}', '')::double precision,
        NULLIF(r.data #>> '{externalPoolBilling,reportedCostUsd}', '')::double precision,
        0
    )), 0)::double precision,
    COALESCE(SUM(COALESCE(
        NULLIF(r.data #>> '{externalPoolBilling,profitUsd}', '')::double precision,
        COALESCE(
            NULLIF(r.data #>> '{externalPoolBilling,upliftedCostUsd}', '')::double precision,
            NULLIF(r.data #>> '{externalPoolBilling,reportedCostUsd}', '')::double precision,
            0
        ) - COALESCE(NULLIF(r.data #>> '{externalPoolBilling,rawCostUsd}', '')::double precision, 0)
    )), 0)::double precision,
    COALESCE(SUM(NULLIF(r.data #>> '{externalPoolBilling,reportedCostUsd}', '')::double precision), 0)::double precision,
    COALESCE(SUM(NULLIF(r.data #>> '{externalPoolBilling,billableCostUsd}', '')::double precision), 0)::double precision,
    COALESCE(SUM(NULLIF(r.data #>> '{externalPoolBilling,costFloorDeltaUsd}', '')::double precision), 0)::double precision,
    COALESCE(SUM(r.duration_ms), 0)::bigint,
    COUNT(*)::bigint,
    COALESCE(MAX(r.duration_ms), 0)::bigint
FROM usage_records r
CROSS JOIN LATERAL (
    VALUES
        ('global', 'all', NULL::text),
        ('status', r.status, NULL::text),
        ('usage_source', r.usage_source, NULL::text),
        ('model', COALESCE(NULLIF(r.model, ''), 'unknown'), NULL::text),
        ('endpoint', COALESCE(NULLIF(r.endpoint, ''), 'unknown'), NULL::text),
        ('credential', r.credential_id::text, NULLIF(r.credential_label, '')),
        ('external_pool', r.data->>'externalPoolId', NULLIF(r.data->>'externalPoolName', '')),
        ('conversation', r.conversation_id, NULL::text),
        ('error',
            CASE WHEN r.status <> 'success'
                 THEN COALESCE(NULLIF(r.error_type, ''), r.status, 'error')
            END,
            CASE WHEN r.status <> 'success' THEN NULLIF(r.error_message, '') END)
) AS d(dimension, dimension_key, dimension_label)
WHERE r.deleted_at IS NULL
  AND d.dimension_key IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM usage_rollup_totals)
GROUP BY d.dimension, d.dimension_key
ON CONFLICT (dimension, dimension_key) DO NOTHING;

INSERT INTO usage_rollup_time_buckets (
    bucket_start, dimension, dimension_key, dimension_label, requests, success_requests,
    error_requests, stream_requests, non_stream_requests, priced_requests, unpriced_requests,
    local_prompt_cache_requests, simulated_requests, upstream_metadata_requests,
    sticky_bound_requests, fallback_from_sticky_requests, total_input_tokens,
    billable_input_tokens, total_output_tokens, total_cache_read_input_tokens,
    total_cache_creation_input_tokens, local_prompt_cache_input_tokens,
    local_prompt_cache_read_input_tokens, local_prompt_cache_creation_input_tokens,
    total_estimated_cost_usd, external_pool_requests, external_pool_priced_requests,
    external_pool_unpriced_requests, external_pool_cost_floor_applied_requests,
    external_pool_raw_cost_usd, external_pool_shaped_cost_usd,
    external_pool_uplifted_cost_usd, external_pool_profit_usd,
    external_pool_reported_cost_usd, external_pool_billable_cost_usd,
    external_pool_cost_floor_delta_usd,
    duration_ms_sum, duration_ms_count, duration_ms_max
)
SELECT
    date_trunc('hour', r.created_at) AS bucket_start,
    d.dimension,
    d.dimension_key,
    MAX(d.dimension_label),
    COUNT(*)::bigint,
    COUNT(*) FILTER (WHERE r.status = 'success')::bigint,
    COUNT(*) FILTER (WHERE r.status <> 'success')::bigint,
    COUNT(*) FILTER (WHERE r.stream)::bigint,
    COUNT(*) FILTER (WHERE NOT r.stream)::bigint,
    COUNT(*) FILTER (WHERE r.pricing_available)::bigint,
    COUNT(*) FILTER (WHERE NOT r.pricing_available)::bigint,
    COUNT(*) FILTER (WHERE r.usage_source = 'local_prompt_cache')::bigint,
    COUNT(*) FILTER (WHERE r.simulated)::bigint,
    COUNT(*) FILTER (WHERE r.usage_source = 'upstream_metadata')::bigint,
    COUNT(*) FILTER (WHERE r.sticky_bound)::bigint,
    COUNT(*) FILTER (WHERE r.fallback_from_sticky)::bigint,
    COALESCE(SUM(r.total_input_tokens), 0)::bigint,
    COALESCE(SUM(r.billable_input_tokens), 0)::bigint,
    COALESCE(SUM(r.output_tokens), 0)::bigint,
    COALESCE(SUM(r.cache_read_input_tokens), 0)::bigint,
    COALESCE(SUM(r.cache_creation_input_tokens), 0)::bigint,
    COALESCE(SUM(r.total_input_tokens) FILTER (WHERE r.usage_source = 'local_prompt_cache'), 0)::bigint,
    COALESCE(SUM(r.cache_read_input_tokens) FILTER (WHERE r.usage_source = 'local_prompt_cache'), 0)::bigint,
    COALESCE(SUM(r.cache_creation_input_tokens) FILTER (WHERE r.usage_source = 'local_prompt_cache'), 0)::bigint,
    COALESCE(SUM(r.estimated_cost_usd), 0)::double precision,
    COUNT(*) FILTER (WHERE r.data->>'routeKind' = 'external_pool')::bigint,
    COUNT(*) FILTER (
        WHERE r.data->>'routeKind' = 'external_pool'
          AND r.data #>> '{externalPoolBilling,pricingAvailable}' = 'true'
    )::bigint,
    COUNT(*) FILTER (
        WHERE r.data->>'routeKind' = 'external_pool'
          AND COALESCE(r.data #>> '{externalPoolBilling,pricingAvailable}', 'false') <> 'true'
    )::bigint,
    COUNT(*) FILTER (
        WHERE r.data->>'routeKind' = 'external_pool'
          AND r.data #>> '{externalPoolBilling,costFloorApplied}' = 'true'
    )::bigint,
    COALESCE(SUM(NULLIF(r.data #>> '{externalPoolBilling,rawCostUsd}', '')::double precision), 0)::double precision,
    COALESCE(SUM(COALESCE(
        NULLIF(r.data #>> '{externalPoolBilling,shapedCostUsd}', '')::double precision,
        NULLIF(r.data #>> '{externalPoolBilling,reportedCostUsd}', '')::double precision,
        0
    )), 0)::double precision,
    COALESCE(SUM(COALESCE(
        NULLIF(r.data #>> '{externalPoolBilling,upliftedCostUsd}', '')::double precision,
        NULLIF(r.data #>> '{externalPoolBilling,reportedCostUsd}', '')::double precision,
        0
    )), 0)::double precision,
    COALESCE(SUM(COALESCE(
        NULLIF(r.data #>> '{externalPoolBilling,profitUsd}', '')::double precision,
        COALESCE(
            NULLIF(r.data #>> '{externalPoolBilling,upliftedCostUsd}', '')::double precision,
            NULLIF(r.data #>> '{externalPoolBilling,reportedCostUsd}', '')::double precision,
            0
        ) - COALESCE(NULLIF(r.data #>> '{externalPoolBilling,rawCostUsd}', '')::double precision, 0)
    )), 0)::double precision,
    COALESCE(SUM(NULLIF(r.data #>> '{externalPoolBilling,reportedCostUsd}', '')::double precision), 0)::double precision,
    COALESCE(SUM(NULLIF(r.data #>> '{externalPoolBilling,billableCostUsd}', '')::double precision), 0)::double precision,
    COALESCE(SUM(NULLIF(r.data #>> '{externalPoolBilling,costFloorDeltaUsd}', '')::double precision), 0)::double precision,
    COALESCE(SUM(r.duration_ms), 0)::bigint,
    COUNT(*)::bigint,
    COALESCE(MAX(r.duration_ms), 0)::bigint
FROM usage_records r
CROSS JOIN LATERAL (
    VALUES
        ('global', 'all', NULL::text),
        ('status', r.status, NULL::text),
        ('usage_source', r.usage_source, NULL::text),
        ('model', COALESCE(NULLIF(r.model, ''), 'unknown'), NULL::text),
        ('endpoint', COALESCE(NULLIF(r.endpoint, ''), 'unknown'), NULL::text),
        ('credential', r.credential_id::text, NULLIF(r.credential_label, '')),
        ('external_pool', r.data->>'externalPoolId', NULLIF(r.data->>'externalPoolName', '')),
        ('error',
            CASE WHEN r.status <> 'success'
                 THEN COALESCE(NULLIF(r.error_type, ''), r.status, 'error')
            END,
            CASE WHEN r.status <> 'success' THEN NULLIF(r.error_message, '') END)
) AS d(dimension, dimension_key, dimension_label)
WHERE r.deleted_at IS NULL
  AND d.dimension_key IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM usage_rollup_time_buckets)
GROUP BY date_trunc('hour', r.created_at), d.dimension, d.dimension_key
ON CONFLICT (bucket_start, dimension, dimension_key) DO NOTHING;

INSERT INTO usage_cache_read_totals (cache_read_input_tokens, requests)
SELECT GREATEST(cache_read_input_tokens, 0), COUNT(*)::bigint
FROM usage_records
WHERE deleted_at IS NULL
  AND NOT EXISTS (SELECT 1 FROM usage_cache_read_totals)
GROUP BY GREATEST(cache_read_input_tokens, 0)
ON CONFLICT (cache_read_input_tokens) DO NOTHING;

INSERT INTO usage_cache_read_rollup_time_buckets (bucket_start, cache_read_input_tokens, requests)
SELECT date_trunc('hour', created_at), GREATEST(cache_read_input_tokens, 0), COUNT(*)::bigint
FROM usage_records
WHERE deleted_at IS NULL
  AND NOT EXISTS (SELECT 1 FROM usage_cache_read_rollup_time_buckets)
GROUP BY date_trunc('hour', created_at), GREATEST(cache_read_input_tokens, 0)
ON CONFLICT (bucket_start, cache_read_input_tokens) DO NOTHING;

INSERT INTO usage_duration_rollup_time_buckets (bucket_start, duration_ms, requests)
SELECT date_trunc('hour', created_at), LEAST(GREATEST(duration_ms, 0), 2147483647)::integer, COUNT(*)::bigint
FROM usage_records
WHERE deleted_at IS NULL
  AND NOT EXISTS (SELECT 1 FROM usage_duration_rollup_time_buckets)
GROUP BY date_trunc('hour', created_at), LEAST(GREATEST(duration_ms, 0), 2147483647)::integer
ON CONFLICT (bucket_start, duration_ms) DO NOTHING;

INSERT INTO usage_credential_cost_summary (
    credential_id, requests, estimated_cost_usd, priced_requests, unpriced_requests
)
SELECT
    credential_id,
    COUNT(*)::bigint,
    COALESCE(SUM(estimated_cost_usd), 0)::double precision,
    COUNT(*) FILTER (WHERE pricing_available)::bigint,
    COUNT(*) FILTER (WHERE NOT pricing_available)::bigint
FROM usage_records
WHERE credential_id IS NOT NULL
  AND deleted_at IS NULL
  AND NOT EXISTS (SELECT 1 FROM usage_credential_cost_summary)
GROUP BY credential_id
ON CONFLICT (credential_id) DO NOTHING;

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
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

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
    total_estimated_cost_usd, external_pool_requests,
    external_pool_priced_requests, external_pool_unpriced_requests,
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
    total_estimated_cost_usd, external_pool_requests,
    external_pool_priced_requests, external_pool_unpriced_requests,
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
    use std::collections::HashMap;
    use std::sync::Arc;

    use super::*;
    use crate::anthropic::pricing::{ModelPriceItem, ModelPricing};
    use crate::anthropic::usage::{
        ExternalPoolBilling, ExternalPoolUsageSnapshot, UsageRecordStatus, UsageRouteKind,
        UsageRouteSubtype, UsageSource,
    };
    use crate::model::config::{ReportedUsageFieldPolicy, ReportedUsagePathPolicy};

    fn test_config() -> Option<Config> {
        let url = std::env::var("KIRO_RS_TEST_POSTGRES_URL").ok()?;
        let mut config = Config::default();
        config.postgres.url = Some(url);
        config.postgres.max_connections = 2;
        Some(config)
    }

    async fn clean(store: &PostgresStore) {
        for statement in [
            "TRUNCATE TABLE admin_audit_logs",
            "TRUNCATE TABLE credential_events",
            "TRUNCATE TABLE usage_credential_cost_summary",
            "TRUNCATE TABLE usage_duration_rollup_time_buckets",
            "TRUNCATE TABLE usage_cache_read_rollup_time_buckets",
            "TRUNCATE TABLE usage_cache_read_totals",
            "TRUNCATE TABLE usage_rollup_time_buckets",
            "TRUNCATE TABLE usage_rollup_totals",
            "TRUNCATE TABLE usage_records",
            "TRUNCATE TABLE model_pricing_sync_status",
            "TRUNCATE TABLE model_pricing",
            "TRUNCATE TABLE credential_runtime_state CASCADE",
            "TRUNCATE TABLE credential_stats CASCADE",
            "TRUNCATE TABLE credentials CASCADE",
            "TRUNCATE TABLE proxy_resources CASCADE",
            "TRUNCATE TABLE runtime_config",
        ] {
            sqlx::query(statement).execute(store.pool()).await.unwrap();
        }
    }

    fn usage_record(id: &str, cache_read: i32) -> UsageRecord {
        UsageRecord {
            id: id.to_string(),
            created_at: Utc::now().to_rfc3339(),
            endpoint: "/v1/messages".to_string(),
            stream: false,
            model: "claude-sonnet-4-5".to_string(),
            upstream_model: None,
            model_resolution_source: None,
            model_resolution_note: None,
            conversation_id: Some("session-a".to_string()),
            credential_id: Some(7),
            credential_label: Some("alpha@example.com".to_string()),
            status: UsageRecordStatus::Success,
            usage_source: UsageSource::LocalPromptCache,
            total_input_tokens: 100,
            compat_input_tokens: 10,
            billable_input_tokens: 10,
            output_tokens: 20,
            cache_read_input_tokens: cache_read,
            cache_creation_input_tokens: 5,
            cache_creation_5m_input_tokens: 5,
            cache_creation_1h_input_tokens: 0,
            estimated_cost_usd: 0.001,
            pricing_available: true,
            pricing_model: Some("claude-sonnet-4-5".to_string()),
            duration_ms: 30,
            first_token_latency_ms: None,
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
            payload_breakdown: None,
            payload_guard_report: None,
        }
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
    async fn postgres_migration_compresses_second_rollup_buckets_to_hours() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
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

        store
            .compress_usage_rollups_to_hour_buckets()
            .await
            .unwrap();

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
            upstream_model: Some("claude-sonnet-4-5".to_string()),
            model_resolution_source: Some("exact".to_string()),
            model_resolution_note: None,
            conversation_id: Some(format!("external-session-{}", id)),
            credential_id: None,
            credential_label: None,
            status: UsageRecordStatus::Success,
            usage_source: UsageSource::UpstreamMetadata,
            total_input_tokens: reported_usage.total_input_tokens,
            compat_input_tokens: reported_usage.input_tokens,
            billable_input_tokens: reported_usage.billable_input_tokens,
            output_tokens: reported_usage.output_tokens,
            cache_read_input_tokens: reported_usage.cache_read_input_tokens,
            cache_creation_input_tokens: reported_usage.cache_creation_input_tokens,
            cache_creation_5m_input_tokens: reported_usage.cache_creation_5m_input_tokens,
            cache_creation_1h_input_tokens: reported_usage.cache_creation_1h_input_tokens,
            estimated_cost_usd: uplifted_cost_usd,
            pricing_available: true,
            pricing_model: Some("claude-sonnet-4-5".to_string()),
            duration_ms: 50,
            first_token_latency_ms: None,
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
            }),
            credential_attempts: Vec::new(),
            error_type: None,
            error_message: None,
            error_detail: None,
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

        let usage_store = PostgresUsageStore::new(Arc::new(store.clone()));
        usage_store
            .record(usage_record("usage-1", 10))
            .await
            .unwrap();
        let mut usage_2 = usage_record("usage-2", 20);
        usage_2.status = UsageRecordStatus::Error;
        usage_2.model = "claude-opus-4-5".to_string();
        usage_2.conversation_id = Some("session-b".to_string());
        usage_2.estimated_cost_usd = 0.0;
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
        assert_eq!(summary.realtime.rpm, 2.0);
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

        usage_store.clear().await.unwrap();
        let cleared_summary = usage_store.summary(15).await.unwrap();
        assert_eq!(cleared_summary.total_requests, 2);
        assert_eq!(cleared_summary.success_requests, 1);
        assert_eq!(cleared_summary.error_requests, 1);
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
        usage_store
            .record(usage_record("usage-1", 30))
            .await
            .unwrap();
        let restored = usage_store
            .query(UsageRecordQuery {
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(restored.total, 1);
        assert_eq!(restored.records[0].id, "usage-1");
        let restored_summary = usage_store.summary(15).await.unwrap();
        assert_eq!(restored_summary.total_requests, 2);
        assert_eq!(restored_summary.high_cache_requests, 2);

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
        assert_eq!(loaded_capabilities.models[0].model, "claude-sonnet-4-9");
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
    async fn postgres_rolls_up_external_pool_billing_for_large_samples_and_keeps_after_cleanup() {
        let Some(config) = test_config() else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let store = PostgresStore::connect_test(&config).await.unwrap();
        clean(&store).await;
        let usage_store = PostgresUsageStore::new(Arc::new(store.clone()));

        for index in 0..1000 {
            let (raw_cost, shaped_cost, uplifted_cost) = if index % 2 == 0 {
                (0.010, 0.006, 0.008)
            } else {
                (0.005, 0.007, 0.009)
            };
            usage_store
                .record(external_usage_record(
                    &format!("external-usage-{index:04}"),
                    raw_cost,
                    shaped_cost,
                    uplifted_cost,
                ))
                .await
                .unwrap();
        }

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

        usage_store.clear().await.unwrap();
        let cleared_page = usage_store
            .query(UsageRecordQuery {
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(cleared_page.total, 0);
        let cleared_summary = usage_store.summary(1_000).await.unwrap();
        assert_eq!(cleared_summary.external_pool_billing.requests, 1000);
        assert!((cleared_summary.external_pool_billing.billable_cost_usd - 8.5).abs() < 0.000001);
        assert!((cleared_summary.external_pool_billing.profit_usd - 1.0).abs() < 0.000001);

        store.drop_test_schema().await.unwrap();
    }
}
