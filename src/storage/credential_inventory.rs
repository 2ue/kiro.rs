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
     updated_at, group_name, origin_credential_id, origin_reason";

/// Group filter value for rows without a group.
pub const UNGROUPED_FILTER: &str = "__none__";
/// Longest group name accepted, in characters.
pub const MAX_GROUP_NAME_CHARS: usize = 64;

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
    pub group_name: Option<String>,
    /// Pool account the row was moved from, when it came out of the pool.
    pub origin_credential_id: Option<u64>,
    /// Disabled reason the pool account had when it was moved here.
    pub origin_reason: Option<String>,
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
    pub group_name: Option<&'a str>,
}

/// A pool account being moved into the inventory.
#[derive(Debug, Clone)]
pub struct MovedInventoryRow<'a> {
    pub auth_kind: &'a str,
    pub refresh_token_hash: Option<&'a str>,
    pub api_key_hash: Option<&'a str>,
    pub identity_key: Option<&'a str>,
    pub email: Option<&'a str>,
    pub data: &'a serde_json::Value,
    pub source: Option<&'a str>,
    pub group_name: Option<&'a str>,
    /// Last quota and subscription snapshot the pool had, so the card has data at once.
    pub account_info: Option<&'a serde_json::Value>,
    pub account_info_at: Option<DateTime<Utc>>,
    pub origin_credential_id: u64,
    pub origin_reason: Option<&'a str>,
}

/// Filter on the group column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupFilter {
    Named(String),
    Ungrouped,
}

impl GroupFilter {
    /// `__none__` selects rows without a group; an empty value means no filter.
    pub fn parse(value: Option<&str>) -> Option<Self> {
        let value = value.map(str::trim).filter(|value| !value.is_empty())?;
        if value == UNGROUPED_FILTER {
            Some(Self::Ungrouped)
        } else {
            Some(Self::Named(value.to_string()))
        }
    }

    fn binds(filter: Option<&Self>) -> (Option<&str>, bool) {
        match filter {
            Some(Self::Named(name)) => (Some(name.as_str()), false),
            Some(Self::Ungrouped) => (None, true),
            None => (None, false),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct InventoryListFilter {
    pub status: Option<String>,
    pub query: Option<String>,
    pub group: Option<GroupFilter>,
    pub limit: i64,
    pub offset: i64,
}

/// Row counts of one group; `name` is `None` for rows without a group.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InventoryGroupCounts {
    pub name: Option<String>,
    pub counts: InventoryCounts,
}

/// A live row matching an account that is about to be moved in from the pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveMatch {
    pub id: u64,
    pub status: String,
    pub pushed_credential_id: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
        group_name: row.try_get("group_name")?,
        origin_credential_id: row
            .try_get::<Option<i64>, _>("origin_credential_id")?
            .map(|id| id as u64),
        origin_reason: row.try_get("origin_reason")?,
    })
}

/// Filter of `list`, `export_rows` and their counts. Binds: $1 status, $2 search pattern,
/// $3 searched id, $4 group name, $5 rows without a group only.
const LIST_WHERE_SQL: &str = "deleted_at IS NULL \
     AND ($1::text IS NULL OR status = $1) \
     AND ($2::text IS NULL OR email ILIKE $2 OR note ILIKE $2 OR source ILIKE $2 \
          OR group_name ILIKE $2 \
          OR ($3::bigint IS NOT NULL AND (id = $3 OR pushed_credential_id = $3 \
              OR origin_credential_id = $3))) \
     AND ($4::text IS NULL OR group_name = $4) \
     AND (NOT $5 OR group_name IS NULL)";

fn search_pattern(query: Option<&str>) -> Option<String> {
    query.map(str::trim).filter(|q| !q.is_empty()).map(|q| {
        format!(
            "%{}%",
            q.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        )
    })
}

fn search_id(query: Option<&str>) -> Option<i64> {
    query
        .map(|q| q.trim().trim_start_matches('#'))
        .and_then(|q| q.parse::<i64>().ok())
}

impl InventoryCounts {
    fn add(&mut self, status: &str, n: u64) {
        self.total += n;
        match status {
            STATUS_STORED => self.stored += n,
            STATUS_INVALID => self.invalid += n,
            STATUS_PUSHING => self.pushing += n,
            STATUS_PUSHED => self.pushed += n,
            _ => {}
        }
    }
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
               (auth_kind, refresh_token_hash, api_key_hash, identity_key, email, data, source, note, \
                group_name) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING {ROW_COLUMNS}"
        ))
        .bind(new.auth_kind)
        .bind(new.refresh_token_hash)
        .bind(new.api_key_hash)
        .bind(new.identity_key)
        .bind(new.email)
        .bind(new.data)
        .bind(new.source)
        .bind(new.note)
        .bind(new.group_name)
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
        let pattern = search_pattern(filter.query.as_deref());
        let id_query = search_id(filter.query.as_deref());
        let (group, ungrouped) = GroupFilter::binds(filter.group.as_ref());
        let total: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM credential_inventory WHERE {LIST_WHERE_SQL}"
        ))
        .bind(filter.status.as_deref())
        .bind(pattern.as_deref())
        .bind(id_query)
        .bind(group)
        .bind(ungrouped)
        .fetch_one(&self.pool)
        .await?;
        let rows = sqlx::query(&format!(
            "SELECT {ROW_COLUMNS} FROM credential_inventory WHERE {LIST_WHERE_SQL} \
             ORDER BY id DESC LIMIT $6 OFFSET $7"
        ))
        .bind(filter.status.as_deref())
        .bind(pattern.as_deref())
        .bind(id_query)
        .bind(group)
        .bind(ungrouped)
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

    /// Status counts, within one group when `group` is set.
    pub async fn counts(&self, group: Option<&GroupFilter>) -> anyhow::Result<InventoryCounts> {
        let (group, ungrouped) = GroupFilter::binds(group);
        let rows = sqlx::query(
            "SELECT status, count(*) AS n FROM credential_inventory WHERE deleted_at IS NULL \
               AND ($1::text IS NULL OR group_name = $1) AND (NOT $2 OR group_name IS NULL) \
             GROUP BY status",
        )
        .bind(group)
        .bind(ungrouped)
        .fetch_all(&self.pool)
        .await?;
        let mut counts = InventoryCounts::default();
        for row in rows {
            let status: String = row.try_get("status")?;
            let n = row.try_get::<i64, _>("n")?.max(0) as u64;
            counts.add(&status, n);
        }
        Ok(counts)
    }

    /// Every group with its status counts, named groups by name, then the rows without a group.
    pub async fn group_counts(&self) -> anyhow::Result<Vec<InventoryGroupCounts>> {
        let rows = sqlx::query(
            "SELECT group_name, status, count(*) AS n FROM credential_inventory \
             WHERE deleted_at IS NULL GROUP BY group_name, status",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut groups: std::collections::BTreeMap<Option<String>, InventoryCounts> =
            std::collections::BTreeMap::new();
        for row in rows {
            let name: Option<String> = row.try_get("group_name")?;
            let status: String = row.try_get("status")?;
            let n = row.try_get::<i64, _>("n")?.max(0) as u64;
            groups.entry(name).or_default().add(&status, n);
        }
        // `None` sorts first in a BTreeMap; the console lists it last.
        let ungrouped = groups.remove(&None);
        let mut result: Vec<InventoryGroupCounts> = groups
            .into_iter()
            .map(|(name, counts)| InventoryGroupCounts { name, counts })
            .collect();
        if let Some(counts) = ungrouped {
            result.push(InventoryGroupCounts { name: None, counts });
        }
        Ok(result)
    }

    /// Sets (or with `None` clears) the group of the given rows. Returns the rows changed.
    pub async fn set_group(&self, ids: &[u64], group: Option<&str>) -> anyhow::Result<u64> {
        let ids: Vec<i64> = ids.iter().map(|id| *id as i64).collect();
        let result = sqlx::query(
            "UPDATE credential_inventory SET group_name = $2, updated_at = now() \
             WHERE id = ANY($1) AND deleted_at IS NULL AND group_name IS DISTINCT FROM $2",
        )
        .bind(&ids)
        .bind(group)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    /// Moves every row of group `from` to group `to` (`None` dissolves the group).
    pub async fn rename_group(&self, from: &str, to: Option<&str>) -> anyhow::Result<u64> {
        let result = sqlx::query(
            "UPDATE credential_inventory SET group_name = $2, updated_at = now() \
             WHERE group_name = $1 AND deleted_at IS NULL",
        )
        .bind(from)
        .bind(to)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    /// Every live row matching the filter, for export; `ids` restricts it to those rows.
    pub async fn export_rows(
        &self,
        ids: Option<&[u64]>,
        filter: &InventoryListFilter,
    ) -> anyhow::Result<Vec<InventoryRow>> {
        if let Some(ids) = ids {
            let ids: Vec<i64> = ids.iter().map(|id| *id as i64).collect();
            let rows = sqlx::query(&format!(
                "SELECT {ROW_COLUMNS} FROM credential_inventory \
                 WHERE id = ANY($1) AND deleted_at IS NULL ORDER BY id ASC"
            ))
            .bind(&ids)
            .fetch_all(&self.pool)
            .await?;
            return rows.into_iter().map(row_from_pg).collect();
        }
        let pattern = search_pattern(filter.query.as_deref());
        let id_query = search_id(filter.query.as_deref());
        let (group, ungrouped) = GroupFilter::binds(filter.group.as_ref());
        let rows = sqlx::query(&format!(
            "SELECT {ROW_COLUMNS} FROM credential_inventory WHERE {LIST_WHERE_SQL} ORDER BY id ASC"
        ))
        .bind(filter.status.as_deref())
        .bind(pattern.as_deref())
        .bind(id_query)
        .bind(group)
        .bind(ungrouped)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_from_pg).collect()
    }

    /// Live rows that are the same account as a pool account about to be moved in: same secret,
    /// same login, or the row it was pushed from.
    pub async fn find_move_matches(
        &self,
        refresh_token_hash: Option<&str>,
        api_key_hash: Option<&str>,
        identity_key: Option<&str>,
        pool_credential_id: u64,
    ) -> anyhow::Result<Vec<MoveMatch>> {
        let rows = sqlx::query(
            "SELECT id, status, pushed_credential_id FROM credential_inventory \
             WHERE deleted_at IS NULL AND ( \
               ($1::text IS NOT NULL AND refresh_token_hash = $1) \
               OR ($2::text IS NOT NULL AND api_key_hash = $2) \
               OR ($3::text IS NOT NULL AND identity_key = $3) \
               OR pushed_credential_id = $4) \
             ORDER BY id ASC",
        )
        .bind(refresh_token_hash)
        .bind(api_key_hash)
        .bind(identity_key)
        .bind(pool_credential_id as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(MoveMatch {
                    id: row.try_get::<i64, _>("id")? as u64,
                    status: row.try_get("status")?,
                    pushed_credential_id: row
                        .try_get::<Option<i64>, _>("pushed_credential_id")?
                        .map(|id| id as u64),
                })
            })
            .collect()
    }

    /// First step of a move: a new row in `pushing`, so renewal leaves it alone until the pool
    /// account is gone. A crash leaves it to `reconcile_inventory_pushes`, which keeps whichever
    /// side still has the account.
    pub async fn begin_move_insert(
        &self,
        moved: &MovedInventoryRow<'_>,
    ) -> anyhow::Result<InventoryRow> {
        let row = sqlx::query(&format!(
            "INSERT INTO credential_inventory \
               (status, auth_kind, refresh_token_hash, api_key_hash, identity_key, email, data, \
                source, group_name, account_info, last_check_at, origin_credential_id, origin_reason) \
             VALUES ('pushing', $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) \
             RETURNING {ROW_COLUMNS}"
        ))
        .bind(moved.auth_kind)
        .bind(moved.refresh_token_hash)
        .bind(moved.api_key_hash)
        .bind(moved.identity_key)
        .bind(moved.email)
        .bind(moved.data)
        .bind(moved.source)
        .bind(moved.group_name)
        .bind(moved.account_info)
        .bind(moved.account_info.and(moved.account_info_at))
        .bind(moved.origin_credential_id as i64)
        .bind(moved.origin_reason)
        .fetch_one(&self.pool)
        .await?;
        row_from_pg(row)
    }

    /// First step of a move for an account that was pushed from this row earlier: the row comes
    /// back in `pushing` with the pool's current secrets. Only a `pushed` row is revived.
    pub async fn begin_move_revive(
        &self,
        id: u64,
        moved: &MovedInventoryRow<'_>,
    ) -> anyhow::Result<Option<InventoryRow>> {
        let row = sqlx::query(&format!(
            "UPDATE credential_inventory SET status = 'pushing', auth_kind = $2, \
               refresh_token_hash = $3, api_key_hash = $4, identity_key = COALESCE($5, identity_key), \
               email = COALESCE($6, email), data = $7, group_name = $8, \
               account_info = COALESCE($9, account_info), \
               last_check_at = CASE WHEN $9::jsonb IS NULL THEN last_check_at ELSE $10 END, \
               last_check_error = NULL, last_refresh_error = NULL, refresh_failure_count = 0, \
               pushed_credential_id = NULL, pushed_at = NULL, origin_credential_id = $11, \
               origin_reason = $12, updated_at = now() \
             WHERE id = $1 AND deleted_at IS NULL AND status = 'pushed' \
             RETURNING {ROW_COLUMNS}"
        ))
        .bind(id as i64)
        .bind(moved.auth_kind)
        .bind(moved.refresh_token_hash)
        .bind(moved.api_key_hash)
        .bind(moved.identity_key)
        .bind(moved.email)
        .bind(moved.data)
        .bind(moved.group_name)
        .bind(moved.account_info)
        .bind(moved.account_info_at)
        .bind(moved.origin_credential_id as i64)
        .bind(moved.origin_reason)
        .fetch_optional(&self.pool)
        .await?;
        row.map(row_from_pg).transpose()
    }

    /// Last step of a move, after the pool account was deleted: the row is the account's only
    /// copy. `invalid` rows are kept but not renewed.
    pub async fn finish_move(&self, id: u64, invalid: bool) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE credential_inventory \
             SET status = CASE WHEN $2 THEN 'invalid' ELSE 'stored' END, updated_at = now() \
             WHERE id = $1 AND deleted_at IS NULL AND status = 'pushing'",
        )
        .bind(id as i64)
        .bind(invalid)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Undoes a move whose pool delete failed. A new row is removed; a revived row goes back to
    /// pointing at the pool account it was pushed to.
    pub async fn rollback_move(
        &self,
        id: u64,
        revived_from_pool_id: Option<u64>,
    ) -> anyhow::Result<()> {
        match revived_from_pool_id {
            Some(credential_id) => self.finish_push(id, credential_id).await,
            None => {
                sqlx::query(
                    "UPDATE credential_inventory SET deleted_at = now(), data = NULL, updated_at = now() \
                     WHERE id = $1 AND deleted_at IS NULL AND status = 'pushing'",
                )
                .bind(id as i64)
                .execute(&self.pool)
                .await?;
                Ok(())
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::postgres::PostgresStore;
    use serde_json::json;

    async fn test_store() -> Option<(PostgresStore, InventoryStore)> {
        let url = crate::storage::integration_test_url("KIRO_RS_TEST_POSTGRES_URL")?;
        let mut config = crate::model::config::Config::default();
        config.postgres.url = Some(url);
        config.postgres.max_connections = 2;
        let postgres = PostgresStore::connect_test(&config).await.unwrap();
        let store = InventoryStore::new(postgres.pool().clone());
        Some((postgres, store))
    }

    fn new_row<'a>(
        hash: &'a str,
        data: &'a serde_json::Value,
        group: Option<&'a str>,
    ) -> NewInventoryRow<'a> {
        NewInventoryRow {
            auth_kind: "social",
            refresh_token_hash: Some(hash),
            api_key_hash: None,
            identity_key: None,
            email: Some("a@example.com"),
            data,
            source: Some("test"),
            note: None,
            group_name: group,
        }
    }

    fn moved<'a>(
        hash: &'a str,
        data: &'a serde_json::Value,
        info: Option<&'a serde_json::Value>,
        pool_id: u64,
    ) -> MovedInventoryRow<'a> {
        MovedInventoryRow {
            auth_kind: "social",
            refresh_token_hash: Some(hash),
            api_key_hash: None,
            identity_key: Some("social|email|b@example.com"),
            email: Some("b@example.com"),
            data,
            source: Some("账号池"),
            group_name: Some("月底恢复"),
            account_info: info,
            account_info_at: Some(Utc::now()),
            origin_credential_id: pool_id,
            origin_reason: Some("QuotaExceeded"),
        }
    }

    #[tokio::test]
    async fn groups_filter_count_rename_and_dissolve() {
        let Some((postgres, store)) = test_store().await else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let data = json!({"refreshToken": "r"});
        let a = store
            .insert(new_row("h-a", &data, Some("甲")))
            .await
            .unwrap();
        let b = store.insert(new_row("h-b", &data, None)).await.unwrap();
        let c = store
            .insert(new_row("h-c", &data, Some("甲")))
            .await
            .unwrap();
        assert_eq!(a.group_name.as_deref(), Some("甲"));

        let only = |group| InventoryListFilter {
            group: Some(group),
            limit: 100,
            ..InventoryListFilter::default()
        };
        let (rows, total) = store
            .list(&only(GroupFilter::Named("甲".into())))
            .await
            .unwrap();
        assert_eq!(total, 2);
        assert!(
            rows.iter()
                .all(|row| row.group_name.as_deref() == Some("甲"))
        );
        let (rows, _) = store.list(&only(GroupFilter::Ungrouped)).await.unwrap();
        assert_eq!(
            rows.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![b.id]
        );
        // The search box finds the group name too.
        let (rows, _) = store
            .list(&InventoryListFilter {
                query: Some("甲".into()),
                limit: 100,
                ..InventoryListFilter::default()
            })
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);

        assert_eq!(store.set_group(&[b.id], Some("乙")).await.unwrap(), 1);
        assert_eq!(store.set_group(&[b.id], Some("乙")).await.unwrap(), 0);
        let groups = store.group_counts().await.unwrap();
        assert_eq!(
            groups
                .iter()
                .map(|group| (group.name.clone(), group.counts.stored))
                .collect::<Vec<_>>(),
            vec![(Some("乙".to_string()), 1), (Some("甲".to_string()), 2)]
        );
        let counts = store
            .counts(Some(&GroupFilter::Named("甲".into())))
            .await
            .unwrap();
        assert_eq!((counts.total, counts.stored), (2, 2));

        assert_eq!(store.rename_group("甲", Some("丙")).await.unwrap(), 2);
        assert_eq!(store.rename_group("丙", None).await.unwrap(), 2);
        let groups = store.group_counts().await.unwrap();
        assert_eq!(
            groups.last().unwrap().name,
            None,
            "ungrouped is listed last"
        );
        assert_eq!(groups.last().unwrap().counts.total, 2);
        assert_eq!(store.get(c.id).await.unwrap().unwrap().group_name, None);

        let exported = store
            .export_rows(Some(&[c.id, a.id]), &InventoryListFilter::default())
            .await
            .unwrap();
        assert_eq!(
            exported.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![a.id, c.id]
        );
        let exported = store
            .export_rows(
                None,
                &InventoryListFilter {
                    group: Some(GroupFilter::Named("乙".into())),
                    ..InventoryListFilter::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(exported.len(), 1);
        postgres.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn moves_insert_finish_and_roll_back() {
        let Some((postgres, store)) = test_store().await else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let data = json!({"refreshToken": "r-move"});
        let info = json!({"usageLimit": 100.0, "currentUsage": 100.0});
        let row = store
            .begin_move_insert(&moved("h-move", &data, Some(&info), 41))
            .await
            .unwrap();
        assert_eq!(row.status, STATUS_PUSHING);
        assert_eq!(row.origin_credential_id, Some(41));
        assert_eq!(row.origin_reason.as_deref(), Some("QuotaExceeded"));
        assert_eq!(row.group_name.as_deref(), Some("月底恢复"));
        assert!(
            row.last_check_at.is_some(),
            "the pool's quota snapshot is kept"
        );
        let matches = store
            .find_move_matches(Some("h-move"), None, None, 999)
            .await
            .unwrap();
        assert_eq!(matches.len(), 1);

        store.finish_move(row.id, false).await.unwrap();
        assert_eq!(
            store.get(row.id).await.unwrap().unwrap().status,
            STATUS_STORED
        );

        // A failed pool delete removes a freshly inserted row.
        let data = json!({"refreshToken": "r-fail"});
        let failed = store
            .begin_move_insert(&moved("h-fail", &data, None, 42))
            .await
            .unwrap();
        assert!(failed.last_check_at.is_none());
        store.rollback_move(failed.id, None).await.unwrap();
        assert!(store.get(failed.id).await.unwrap().is_none());
        // The rolled-back secret can be moved again (the unique index only covers live rows).
        let again = store
            .begin_move_insert(&moved("h-fail", &data, None, 42))
            .await
            .unwrap();
        store.finish_move(again.id, true).await.unwrap();
        assert_eq!(
            store.get(again.id).await.unwrap().unwrap().status,
            STATUS_INVALID
        );
        postgres.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn an_account_pushed_from_a_row_is_moved_back_into_that_row() {
        let Some((postgres, store)) = test_store().await else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let data = json!({"refreshToken": "r-old"});
        let row = store.insert(new_row("h-old", &data, None)).await.unwrap();
        store.begin_push(row.id).await.unwrap().unwrap();
        store.finish_push(row.id, 77).await.unwrap();

        // The pool rotated the refresh token: only the pushed id still links the two.
        let matches = store
            .find_move_matches(Some("h-rotated"), None, None, 77)
            .await
            .unwrap();
        assert_eq!(
            matches,
            vec![MoveMatch {
                id: row.id,
                status: STATUS_PUSHED.to_string(),
                pushed_credential_id: Some(77),
            }]
        );

        let fresh = json!({"refreshToken": "r-rotated"});
        let revived = store
            .begin_move_revive(row.id, &moved("h-rotated", &fresh, None, 77))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(revived.status, STATUS_PUSHING);
        assert_eq!(revived.data, Some(fresh.clone()));
        assert_eq!(revived.pushed_credential_id, None);
        assert_eq!(revived.refresh_token_hash.as_deref(), Some("h-rotated"));

        // A failed pool delete points the row back at the pool account, without secrets.
        store.rollback_move(row.id, Some(77)).await.unwrap();
        let restored = store.get(row.id).await.unwrap().unwrap();
        assert_eq!(restored.status, STATUS_PUSHED);
        assert_eq!(restored.pushed_credential_id, Some(77));
        assert_eq!(restored.data, None);

        // Only a pushed row is revived.
        store
            .begin_move_revive(row.id, &moved("h-rotated", &fresh, None, 77))
            .await
            .unwrap()
            .unwrap();
        store.finish_move(row.id, false).await.unwrap();
        assert!(
            store
                .begin_move_revive(row.id, &moved("h-rotated", &fresh, None, 77))
                .await
                .unwrap()
                .is_none()
        );
        let stored = store.get(row.id).await.unwrap().unwrap();
        assert_eq!(stored.status, STATUS_STORED);
        assert_eq!(stored.group_name.as_deref(), Some("月底恢复"));
        postgres.drop_test_schema().await.unwrap();
    }

    #[test]
    fn group_filter_parses_the_ungrouped_marker() {
        assert_eq!(GroupFilter::parse(None), None);
        assert_eq!(GroupFilter::parse(Some("  ")), None);
        assert_eq!(
            GroupFilter::parse(Some(UNGROUPED_FILTER)),
            Some(GroupFilter::Ungrouped)
        );
        assert_eq!(
            GroupFilter::parse(Some(" 甲 ")),
            Some(GroupFilter::Named("甲".to_string()))
        );
    }
}
