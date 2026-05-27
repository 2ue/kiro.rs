use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{
    PgPool, Postgres, QueryBuilder, Row, Transaction,
    postgres::{PgPoolOptions, PgRow},
};

use crate::anthropic::model_capabilities::{ModelCapabilitiesStatus, ModelCapabilityItem};
use crate::anthropic::pricing::{ModelPriceItem, ModelPricing, PricingStatus};
use crate::anthropic::usage::{
    CredentialCostSummary, REALTIME_USAGE_WINDOW_SECS, UsageAggregate, UsageRealtimeStats,
    UsageRecord, UsageRecordQuery, UsageRecordStatus, UsageRecordsPageResult, UsageRecordsResult,
    UsageSummary,
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
            store.migrate().await?;
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
        store.migrate().await?;
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

    pub async fn migrate(&self) -> anyhow::Result<()> {
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

            for statement in SCHEMA_SQL.split(";") {
                let statement = statement.trim();
                if !statement.is_empty() {
                    sqlx::query(statement).execute(&mut *conn).await?;
                }
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

    pub async fn find_active_api_key_credential(
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
        if let Some(existing) = self.find_active_api_key_credential(api_key).await? {
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
                .find_active_api_key_credential(api_key)
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
    /// PgSQL 中其他 active 凭据，避免旧进程内存快照覆盖其他实例新增的凭据。
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
            .bind(&status.source)
            .bind(&status.source_url)
            .bind(&status.last_synced_at)
            .bind(&status.last_error)
            .execute(&mut *tx)
            .await?;
        }
        if incoming_models.is_empty() {
            sqlx::query("DELETE FROM model_pricing")
                .execute(&mut *tx)
                .await?;
        } else {
            sqlx::query("DELETE FROM model_pricing WHERE model <> ALL($1::text[])")
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
            .bind(&status.source)
            .bind(&status.last_synced_at)
            .bind(&status.last_error)
            .execute(&mut *tx)
            .await?;
        }
        if incoming_models.is_empty() {
            sqlx::query("DELETE FROM model_capabilities")
                .execute(&mut *tx)
                .await?;
        } else {
            sqlx::query("DELETE FROM model_capabilities WHERE model <> ALL($1::text[])")
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

impl PostgresUsageStore {
    pub fn new(store: Arc<PostgresStore>) -> Self {
        Self { store }
    }

    pub async fn record(&self, record: UsageRecord) -> anyhow::Result<()> {
        let value = serde_json::to_value(&record)?;
        let created_at = chrono::DateTime::parse_from_rfc3339(&record.created_at)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
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
        .execute(self.store.pool())
        .await?;
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

    pub async fn summary(&self, high_cache_threshold: i32) -> anyhow::Result<UsageSummary> {
        let row = sqlx::query(
            r#"
            SELECT
                COUNT(*)::bigint AS total_requests,
                COUNT(*) FILTER (WHERE status = 'success')::bigint AS success_requests,
                COUNT(*) FILTER (WHERE status <> 'success')::bigint AS error_requests,
                COUNT(*) FILTER (WHERE cache_read_input_tokens >= $1)::bigint AS high_cache_requests,
                COALESCE(SUM(total_input_tokens), 0)::bigint AS total_input_tokens,
                COALESCE(SUM(output_tokens), 0)::bigint AS total_output_tokens,
                COALESCE(SUM(cache_read_input_tokens), 0)::bigint AS total_cache_read_input_tokens,
                COALESCE(SUM(cache_creation_input_tokens), 0)::bigint AS total_cache_creation_input_tokens,
                COALESCE(SUM(estimated_cost_usd), 0)::double precision AS total_estimated_cost_usd,
                COUNT(*) FILTER (WHERE pricing_available)::bigint AS priced_requests,
                COUNT(*) FILTER (WHERE NOT pricing_available)::bigint AS unpriced_requests,
                COUNT(*) FILTER (WHERE usage_source = 'local_prompt_cache')::bigint AS local_prompt_cache_requests,
                COALESCE(SUM(total_input_tokens) FILTER (WHERE usage_source = 'local_prompt_cache'), 0)::bigint AS local_prompt_cache_input_tokens,
                COALESCE(SUM(cache_read_input_tokens) FILTER (WHERE usage_source = 'local_prompt_cache'), 0)::bigint AS local_prompt_cache_read_input_tokens,
                COALESCE(SUM(cache_creation_input_tokens) FILTER (WHERE usage_source = 'local_prompt_cache'), 0)::bigint AS local_prompt_cache_creation_input_tokens,
                COUNT(*) FILTER (WHERE simulated)::bigint AS simulated_requests,
                COUNT(*) FILTER (WHERE usage_source = 'upstream_metadata')::bigint AS upstream_metadata_requests,
                COUNT(*) FILTER (WHERE created_at >= now() - interval '60 seconds')::bigint AS realtime_requests,
                COALESCE(SUM(total_input_tokens) FILTER (WHERE created_at >= now() - interval '60 seconds'), 0)::bigint AS realtime_input_tokens,
                COALESCE(SUM(output_tokens) FILTER (WHERE created_at >= now() - interval '60 seconds'), 0)::bigint AS realtime_output_tokens,
                COALESCE(SUM(billable_input_tokens) FILTER (WHERE created_at >= now() - interval '60 seconds'), 0)::bigint AS realtime_billable_input_tokens
            FROM usage_records
            WHERE deleted_at IS NULL
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

    pub async fn credential_cost_summary(
        &self,
    ) -> anyhow::Result<HashMap<u64, CredentialCostSummary>> {
        let rows = sqlx::query(
            r#"
            SELECT
                credential_id,
                COALESCE(SUM(estimated_cost_usd), 0)::double precision AS estimated_cost_usd,
                COUNT(*) FILTER (WHERE pricing_available)::bigint AS priced_requests,
                COUNT(*) FILTER (WHERE NOT pricing_available)::bigint AS unpriced_requests
            FROM usage_records
            WHERE credential_id IS NOT NULL AND deleted_at IS NULL
            GROUP BY credential_id
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
                credential_id::text AS key,
                MAX(credential_label) AS label,
                COUNT(*)::bigint AS requests,
                COALESCE(SUM(cache_read_input_tokens), 0)::bigint AS cache_read_input_tokens,
                COALESCE(SUM(cache_creation_input_tokens), 0)::bigint AS cache_creation_input_tokens,
                COALESCE(SUM(estimated_cost_usd), 0)::double precision AS estimated_cost_usd
            FROM usage_records
            WHERE credential_id IS NOT NULL AND deleted_at IS NULL
            GROUP BY credential_id
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
                conversation_id AS key,
                NULL::text AS label,
                COUNT(*)::bigint AS requests,
                COALESCE(SUM(cache_read_input_tokens), 0)::bigint AS cache_read_input_tokens,
                COALESCE(SUM(cache_creation_input_tokens), 0)::bigint AS cache_creation_input_tokens,
                COALESCE(SUM(estimated_cost_usd), 0)::double precision AS estimated_cost_usd
            FROM usage_records
            WHERE conversation_id IS NOT NULL AND deleted_at IS NULL
            GROUP BY conversation_id
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

fn row_i64_to_usize(row: &PgRow, column: &str) -> anyhow::Result<usize> {
    let value: i64 = row.try_get(column)?;
    Ok(value.max(0) as usize)
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use super::*;
    use crate::anthropic::pricing::{ModelPriceItem, ModelPricing};
    use crate::anthropic::usage::{UsageRecordStatus, UsageSource};
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
            simulated: true,
            sticky_bound: true,
            fallback_from_sticky: false,
            credential_attempts: Vec::new(),
            error_type: None,
            error_message: None,
            error_detail: None,
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
            "保存旧快照不应软删除数据库中其他 active 凭据"
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

        let cost_summary = usage_store.credential_cost_summary().await.unwrap();
        let cost_summary = cost_summary.get(&7).unwrap();
        assert_eq!(cost_summary.priced_requests, 1);
        assert_eq!(cost_summary.unpriced_requests, 1);
        assert!((cost_summary.estimated_cost_usd - 0.001).abs() < f64::EPSILON);

        usage_store.clear().await.unwrap();
        let cleared_summary = usage_store.summary(15).await.unwrap();
        assert_eq!(cleared_summary.total_requests, 0);
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
}
