//! Account inventory storage: accounts kept for later use, outside the scheduler.
//!
//! The table lives in the shared PostgreSQL database but nothing in the scheduler, token manager
//! or usage pipeline reads it. While an account is `stored`, the inventory owns its secrets and
//! renews its token. Pushing an account moves the secrets to the pool: the row keeps only the
//! identity and the id of the pool account, so a refresh token is never held in two places.

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};

/// The account is kept here; the inventory owns its secrets and renews its token.
pub const STATUS_STORED: &str = "stored";
/// The refresh token was rejected; renewal stopped.
pub const STATUS_INVALID: &str = "invalid";
/// Being moved to the pool; renewal skips it.
pub const STATUS_PUSHING: &str = "pushing";
/// Moved to the pool; secrets cleared, `pushed_credential_id` points at the pool account.
pub const STATUS_PUSHED: &str = "pushed";

const ROW_COLUMNS: &str = "id, status, auth_kind, refresh_token_hash, api_key_hash, identity_key, \
     email, data, source, note, account_info, last_check_at, last_check_error, last_refresh_at, \
     last_refresh_error, refresh_failure_count, pushed_credential_id, pushed_at, created_at, \
     updated_at";

#[derive(Debug, Clone)]
pub struct InventoryRow {
    pub id: u64,
    pub status: String,
    pub auth_kind: String,
    pub refresh_token_hash: Option<String>,
    pub api_key_hash: Option<String>,
    /// Login identity (auth method + e-mail or profile ARN), stable across token rotation.
    #[allow(dead_code)]
    pub identity_key: Option<String>,
    pub email: Option<String>,
    /// Full credential JSON while stored; `None` once pushed.
    pub data: Option<serde_json::Value>,
    pub source: Option<String>,
    pub note: Option<String>,
    pub account_info: Option<serde_json::Value>,
    pub last_check_at: Option<DateTime<Utc>>,
    pub last_check_error: Option<String>,
    pub last_refresh_at: Option<DateTime<Utc>>,
    pub last_refresh_error: Option<String>,
    pub refresh_failure_count: i32,
    pub pushed_credential_id: Option<u64>,
    pub pushed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewInventoryRow<'a> {
    pub auth_kind: &'a str,
    pub refresh_token_hash: Option<&'a str>,
    pub api_key_hash: Option<&'a str>,
    pub identity_key: Option<&'a str>,
    pub email: Option<&'a str>,
    pub data: &'a serde_json::Value,
    pub source: Option<&'a str>,
    pub note: Option<&'a str>,
}

#[derive(Debug, Clone, Default)]
pub struct InventoryListFilter {
    pub status: Option<String>,
    pub query: Option<String>,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Debug, Clone, Default)]
pub struct InventoryCounts {
    pub total: u64,
    pub stored: u64,
    pub invalid: u64,
    pub pushing: u64,
    pub pushed: u64,
}

fn row_from_pg(row: sqlx::postgres::PgRow) -> anyhow::Result<InventoryRow> {
    Ok(InventoryRow {
        id: row.try_get::<i64, _>("id")? as u64,
        status: row.try_get("status")?,
        auth_kind: row.try_get("auth_kind")?,
        refresh_token_hash: row.try_get("refresh_token_hash")?,
        api_key_hash: row.try_get("api_key_hash")?,
        identity_key: row.try_get("identity_key")?,
        email: row.try_get("email")?,
        data: row.try_get("data")?,
        source: row.try_get("source")?,
        note: row.try_get("note")?,
        account_info: row.try_get("account_info")?,
        last_check_at: row.try_get("last_check_at")?,
        last_check_error: row.try_get("last_check_error")?,
        last_refresh_at: row.try_get("last_refresh_at")?,
        last_refresh_error: row.try_get("last_refresh_error")?,
        refresh_failure_count: row.try_get("refresh_failure_count")?,
        pushed_credential_id: row
            .try_get::<Option<i64>, _>("pushed_credential_id")?
            .map(|id| id as u64),
        pushed_at: row.try_get("pushed_at")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// Inventory table access. Holds only the shared pool; it has no other dependency on the store.
#[derive(Clone)]
pub struct InventoryStore {
    pool: PgPool,
}

impl InventoryStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Ids of live rows that match a secret hash or the identity key.
    pub async fn find_duplicates(
        &self,
        refresh_token_hash: Option<&str>,
        api_key_hash: Option<&str>,
        identity_key: Option<&str>,
    ) -> anyhow::Result<Vec<(u64, String)>> {
        let rows = sqlx::query(
            "SELECT id, status FROM credential_inventory WHERE deleted_at IS NULL AND ( \
               ($1::text IS NOT NULL AND refresh_token_hash = $1) \
               OR ($2::text IS NOT NULL AND api_key_hash = $2) \
               OR ($3::text IS NOT NULL AND identity_key = $3))",
        )
        .bind(refresh_token_hash)
        .bind(api_key_hash)
        .bind(identity_key)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| Ok((row.try_get::<i64, _>("id")? as u64, row.try_get("status")?)))
            .collect()
    }

    pub async fn insert(&self, new: NewInventoryRow<'_>) -> anyhow::Result<InventoryRow> {
        let row = sqlx::query(&format!(
            "INSERT INTO credential_inventory \
               (auth_kind, refresh_token_hash, api_key_hash, identity_key, email, data, source, note) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING {ROW_COLUMNS}"
        ))
        .bind(new.auth_kind)
        .bind(new.refresh_token_hash)
        .bind(new.api_key_hash)
        .bind(new.identity_key)
        .bind(new.email)
        .bind(new.data)
        .bind(new.source)
        .bind(new.note)
        .fetch_one(&self.pool)
        .await?;
        row_from_pg(row)
    }

    pub async fn get(&self, id: u64) -> anyhow::Result<Option<InventoryRow>> {
        let row = sqlx::query(&format!(
            "SELECT {ROW_COLUMNS} FROM credential_inventory WHERE id = $1 AND deleted_at IS NULL"
        ))
        .bind(id as i64)
        .fetch_optional(&self.pool)
        .await?;
        row.map(row_from_pg).transpose()
    }

    pub async fn list(
        &self,
        filter: &InventoryListFilter,
    ) -> anyhow::Result<(Vec<InventoryRow>, u64)> {
        let pattern = filter
            .query
            .as_deref()
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .map(|q| {
                format!(
                    "%{}%",
                    q.replace('\\', "\\\\")
                        .replace('%', "\\%")
                        .replace('_', "\\_")
                )
            });
        let id_query = filter
            .query
            .as_deref()
            .map(|q| q.trim().trim_start_matches('#'))
            .and_then(|q| q.parse::<i64>().ok());
        let where_sql = "deleted_at IS NULL \
             AND ($1::text IS NULL OR status = $1) \
             AND ($2::text IS NULL OR email ILIKE $2 OR note ILIKE $2 OR source ILIKE $2 \
                  OR ($3::bigint IS NOT NULL AND (id = $3 OR pushed_credential_id = $3)))";
        let total: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM credential_inventory WHERE {where_sql}"
        ))
        .bind(filter.status.as_deref())
        .bind(pattern.as_deref())
        .bind(id_query)
        .fetch_one(&self.pool)
        .await?;
        let rows = sqlx::query(&format!(
            "SELECT {ROW_COLUMNS} FROM credential_inventory WHERE {where_sql} \
             ORDER BY id DESC LIMIT $4 OFFSET $5"
        ))
        .bind(filter.status.as_deref())
        .bind(pattern.as_deref())
        .bind(id_query)
        .bind(filter.limit)
        .bind(filter.offset)
        .fetch_all(&self.pool)
        .await?;
        Ok((
            rows.into_iter()
                .map(row_from_pg)
                .collect::<anyhow::Result<_>>()?,
            total.max(0) as u64,
        ))
    }

    pub async fn counts(&self) -> anyhow::Result<InventoryCounts> {
        let rows = sqlx::query(
            "SELECT status, count(*) AS n FROM credential_inventory WHERE deleted_at IS NULL GROUP BY status",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut counts = InventoryCounts::default();
        for row in rows {
            let status: String = row.try_get("status")?;
            let n = row.try_get::<i64, _>("n")?.max(0) as u64;
            counts.total += n;
            match status.as_str() {
                STATUS_STORED => counts.stored = n,
                STATUS_INVALID => counts.invalid = n,
                STATUS_PUSHING => counts.pushing = n,
                STATUS_PUSHED => counts.pushed = n,
                _ => {}
            }
        }
        Ok(counts)
    }

    /// Stores a renewed credential. Only applies while the row is still `stored` (or `invalid` for a
    /// manual recheck), so a renewal racing with a push cannot overwrite it.
    pub async fn save_refreshed(
        &self,
        id: u64,
        data: &serde_json::Value,
        refresh_token_hash: Option<&str>,
    ) -> anyhow::Result<bool> {
        let result = sqlx::query(
            "UPDATE credential_inventory SET data = $2, refresh_token_hash = COALESCE($3, refresh_token_hash), \
               status = 'stored', last_refresh_at = now(), last_refresh_error = NULL, \
               refresh_failure_count = 0, updated_at = now() \
             WHERE id = $1 AND deleted_at IS NULL AND status IN ('stored', 'invalid')",
        )
        .bind(id as i64)
        .bind(data)
        .bind(refresh_token_hash)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn record_refresh_failure(
        &self,
        id: u64,
        error: &str,
        invalid: bool,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE credential_inventory SET last_refresh_error = $2, \
               refresh_failure_count = refresh_failure_count + 1, \
               status = CASE WHEN $3 THEN 'invalid' ELSE status END, updated_at = now() \
             WHERE id = $1 AND deleted_at IS NULL AND status IN ('stored', 'invalid')",
        )
        .bind(id as i64)
        .bind(error)
        .bind(invalid)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn save_check(
        &self,
        id: u64,
        account_info: Option<&serde_json::Value>,
        error: Option<&str>,
        email: Option<&str>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE credential_inventory SET account_info = COALESCE($2, account_info), \
               last_check_at = now(), last_check_error = $3, email = COALESCE(email, $4), updated_at = now() \
             WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(id as i64)
        .bind(account_info)
        .bind(error)
        .bind(email)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_settings(
        &self,
        id: u64,
        data: &serde_json::Value,
        note: Option<&str>,
    ) -> anyhow::Result<bool> {
        let result = sqlx::query(
            "UPDATE credential_inventory SET data = $2, note = $3, updated_at = now() \
             WHERE id = $1 AND deleted_at IS NULL AND status IN ('stored', 'invalid')",
        )
        .bind(id as i64)
        .bind(data)
        .bind(note)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Claims a stored row for a push. Fails when another push or a delete got there first.
    pub async fn begin_push(&self, id: u64) -> anyhow::Result<Option<InventoryRow>> {
        let row = sqlx::query(&format!(
            "UPDATE credential_inventory SET status = 'pushing', updated_at = now() \
             WHERE id = $1 AND deleted_at IS NULL AND status IN ('stored', 'invalid') \
             RETURNING {ROW_COLUMNS}"
        ))
        .bind(id as i64)
        .fetch_optional(&self.pool)
        .await?;
        row.map(row_from_pg).transpose()
    }

    /// The pool now owns the account: drop the secrets and keep a pointer to the pool account.
    pub async fn finish_push(&self, id: u64, credential_id: u64) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE credential_inventory SET status = 'pushed', data = NULL, \
               pushed_credential_id = $2, pushed_at = now(), last_check_error = NULL, updated_at = now() \
             WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(id as i64)
        .bind(credential_id as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn abort_push(&self, id: u64, error: &str) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE credential_inventory SET status = 'stored', last_check_error = $2, updated_at = now() \
             WHERE id = $1 AND deleted_at IS NULL AND status = 'pushing'",
        )
        .bind(id as i64)
        .bind(error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Rows left in `pushing` by a crash, older than `stale_secs`.
    pub async fn stale_pushing(&self, stale_secs: i64) -> anyhow::Result<Vec<InventoryRow>> {
        let rows = sqlx::query(&format!(
            "SELECT {ROW_COLUMNS} FROM credential_inventory \
             WHERE deleted_at IS NULL AND status = 'pushing' \
               AND updated_at < now() - make_interval(secs => $1)"
        ))
        .bind(stale_secs as f64)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_from_pg).collect()
    }

    pub async fn soft_delete(&self, id: u64) -> anyhow::Result<bool> {
        let result = sqlx::query(
            "UPDATE credential_inventory SET deleted_at = now(), data = NULL, updated_at = now() \
             WHERE id = $1 AND deleted_at IS NULL AND status <> 'pushing'",
        )
        .bind(id as i64)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Renews due tokens one row at a time. The row stays locked while `renew` runs, so two
    /// instances never refresh the same token (a rotated refresh token would be lost).
    pub async fn renew_due<F, Fut>(
        &self,
        due_before: DateTime<Utc>,
        max_rows: usize,
        mut renew: F,
    ) -> anyhow::Result<(usize, usize)>
    where
        F: FnMut(InventoryRow) -> Fut,
        Fut: std::future::Future<Output = RenewOutcome>,
    {
        let mut renewed = 0;
        let mut failed = 0;
        let mut last_id: i64 = 0;
        for _ in 0..max_rows {
            let mut tx = self.pool.begin().await?;
            let row = sqlx::query(&format!(
                "SELECT {ROW_COLUMNS} FROM credential_inventory \
                 WHERE deleted_at IS NULL AND status = 'stored' AND auth_kind <> 'api_key' \
                   AND data IS NOT NULL AND id > $1 \
                   AND (NULLIF(data->>'expiresAt', '') IS NULL \
                        OR (data->>'expiresAt')::timestamptz <= $2) \
                 ORDER BY id ASC LIMIT 1 FOR UPDATE SKIP LOCKED"
            ))
            .bind(last_id)
            .bind(due_before)
            .fetch_optional(&mut *tx)
            .await?;
            let Some(row) = row else {
                tx.rollback().await?;
                break;
            };
            let row = row_from_pg(row)?;
            last_id = row.id as i64;
            let id = row.id;
            match renew(row).await {
                RenewOutcome::Refreshed {
                    data,
                    refresh_token_hash,
                } => {
                    sqlx::query(
                        "UPDATE credential_inventory SET data = $2, \
                           refresh_token_hash = COALESCE($3, refresh_token_hash), last_refresh_at = now(), \
                           last_refresh_error = NULL, refresh_failure_count = 0, updated_at = now() \
                         WHERE id = $1",
                    )
                    .bind(id as i64)
                    .bind(&data)
                    .bind(refresh_token_hash.as_deref())
                    .execute(&mut *tx)
                    .await?;
                    renewed += 1;
                }
                RenewOutcome::Failed { error, invalid } => {
                    sqlx::query(
                        "UPDATE credential_inventory SET last_refresh_error = $2, \
                           refresh_failure_count = refresh_failure_count + 1, \
                           status = CASE WHEN $3 THEN 'invalid' ELSE status END, updated_at = now() \
                         WHERE id = $1",
                    )
                    .bind(id as i64)
                    .bind(&error)
                    .bind(invalid)
                    .execute(&mut *tx)
                    .await?;
                    failed += 1;
                }
            }
            tx.commit().await?;
        }
        Ok((renewed, failed))
    }
}

pub enum RenewOutcome {
    Refreshed {
        data: serde_json::Value,
        refresh_token_hash: Option<String>,
    },
    Failed {
        error: String,
        invalid: bool,
    },
}
