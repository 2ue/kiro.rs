//! Account inventory storage: accounts kept for later use, outside the scheduler.
//!
//! The table lives in the shared PostgreSQL database but nothing in the scheduler, token manager
//! or usage pipeline reads it. While an account is `stored`, the inventory owns its secrets and
//! renews its token. Pushing an account moves the secrets to the pool: the row keeps only the
//! identity and the id of the pool account, so a refresh token is never held in two places.

use std::collections::HashSet;

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
     updated_at, group_name, origin_credential_id, origin_reason, origin_snapshot, next_renew_at";

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
    /// The pool account's state when it was moved here (status, quota, scheduling parameters).
    pub origin_snapshot: Option<serde_json::Value>,
    /// Renewal after a failed one waits until then.
    pub next_renew_at: Option<DateTime<Utc>>,
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
    pub origin_snapshot: Option<&'a serde_json::Value>,
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

/// Filter on where a row came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OriginFilter {
    /// Moved in from the pool.
    Pool,
    /// Imported (not moved from the pool).
    Imported,
    /// Moved in from the pool with this disabled reason.
    Reason(String),
}

impl OriginFilter {
    pub const POOL: &'static str = "__pool__";
    pub const IMPORTED: &'static str = "__imported__";

    pub fn parse(value: Option<&str>) -> Option<Self> {
        let value = value.map(str::trim).filter(|value| !value.is_empty())?;
        Some(match value {
            Self::POOL => Self::Pool,
            Self::IMPORTED => Self::Imported,
            reason => Self::Reason(reason.to_string()),
        })
    }

    /// ($6 reason, $7 moved from the pool only, $8 imported only).
    fn binds(filter: Option<&Self>) -> (Option<&str>, bool, bool) {
        match filter {
            Some(Self::Pool) => (None, true, false),
            Some(Self::Imported) => (None, false, true),
            Some(Self::Reason(reason)) => (Some(reason.as_str()), false, false),
            None => (None, false, false),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct InventoryListFilter {
    pub status: Option<String>,
    pub query: Option<String>,
    pub group: Option<GroupFilter>,
    pub origin: Option<OriginFilter>,
    /// Subscription titles to match, `""` for rows without one; an empty list matches nothing.
    pub subscription_titles: Option<Vec<String>>,
    /// `remaining`, `exhausted` or `unchecked` (no quota snapshot yet).
    pub quota: Option<String>,
    /// `with` or `without` a proxy (a resource or a direct URL).
    pub proxy: Option<String>,
    pub proxy_resource_id: Option<u64>,
    pub auth_kind: Option<String>,
    pub sort: InventorySort,
    pub ascending: bool,
    pub limit: i64,
    pub offset: i64,
}

/// List order. Every order ends with `id DESC`, so pages stay stable; empty values sort last.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum InventorySort {
    /// Newest rows first.
    #[default]
    Newest,
    /// Quota left at the last check.
    Remaining,
    /// Last quota check.
    Checked,
    /// When the row entered the inventory.
    Created,
    /// When the stored access token expires.
    Expires,
}

impl InventorySort {
    pub fn parse(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            Some("remaining") => Self::Remaining,
            Some("checked") => Self::Checked,
            Some("created") => Self::Created,
            Some("expires") => Self::Expires,
            _ => Self::Newest,
        }
    }

    fn order_by(self, ascending: bool) -> String {
        let expression = match self {
            Self::Newest => return "id DESC".to_string(),
            Self::Remaining => REMAINING_SQL,
            Self::Checked => "last_check_at",
            Self::Created => "created_at",
            // RFC 3339 text sorts in time order and cannot fail to cast.
            Self::Expires => "NULLIF(data->>'expiresAt', '')",
        };
        let direction = if ascending { "ASC" } else { "DESC" };
        format!("{expression} {direction} NULLS LAST, id DESC")
    }
}

/// Quota left at the last check; NULL when the row has no numeric snapshot.
const REMAINING_SQL: &str = "(CASE WHEN jsonb_typeof(account_info->'remaining') = 'number' \
     THEN (account_info->>'remaining')::float8 END)";

/// Quota totals of the filtered rows that hold an account, per subscription title.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InventoryTitleSummary {
    /// `""` for rows without a subscription title.
    pub title: String,
    pub accounts: u64,
    pub unchecked: u64,
    pub exhausted: u64,
    pub remaining: f64,
    pub usage_limit: f64,
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

/// What the rows still holding their account (every live row not pushed out) are matched on
/// by a move: a pool account matching any of these is skipped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeldMoveKeys {
    pub refresh_token_hashes: HashSet<String>,
    pub api_key_hashes: HashSet<String>,
    pub identity_keys: HashSet<String>,
    pub pushed_credential_ids: HashSet<u64>,
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
        origin_snapshot: row.try_get("origin_snapshot")?,
        next_renew_at: row.try_get("next_renew_at")?,
    })
}

/// Filter of `list` and `export_rows`. Binds: $1 status, $2 search pattern, $3 searched id,
/// $4 group name, $5 rows without a group only, $6 origin disabled reason, $7 moved from the
/// pool only, $8 imported only.
const LIST_WHERE_SQL: &str = "deleted_at IS NULL \
     AND ($1::text IS NULL OR status = $1) \
     AND ($2::text IS NULL OR email ILIKE $2 OR note ILIKE $2 OR source ILIKE $2 \
          OR group_name ILIKE $2 \
          OR ($3::bigint IS NOT NULL AND (id = $3 OR pushed_credential_id = $3 \
              OR origin_credential_id = $3))) \
     AND ($4::text IS NULL OR group_name = $4) \
     AND (NOT $5 OR group_name IS NULL) \
     AND ($6::text IS NULL OR (origin_credential_id IS NOT NULL \
          AND COALESCE(origin_reason, '') = CASE WHEN $6 = '__none__' THEN '' ELSE $6 END)) \
     AND (NOT $7 OR origin_credential_id IS NOT NULL) \
     AND (NOT $8 OR origin_credential_id IS NULL) \
     AND ($9::text[] IS NULL OR COALESCE(account_info->>'subscriptionTitle', '') = ANY($9)) \
     AND ($10::text IS NULL \
          OR ($10 = 'unchecked' AND account_info IS NULL) \
          OR ($10 = 'remaining' AND (CASE WHEN jsonb_typeof(account_info->'remaining') = 'number' \
              THEN (account_info->>'remaining')::float8 END) > 0) \
          OR ($10 = 'exhausted' AND (CASE WHEN jsonb_typeof(account_info->'remaining') = 'number' \
              THEN (account_info->>'remaining')::float8 END) <= 0)) \
     AND ($11::text IS NULL \
          OR ($11 = 'with' AND (data ? 'proxyUrl' OR data ? 'proxyResourceId')) \
          OR ($11 = 'without' AND data IS NOT NULL \
              AND NOT (data ? 'proxyUrl' OR data ? 'proxyResourceId'))) \
     AND ($12::bigint IS NULL OR data->'proxyResourceId' = to_jsonb($12::bigint)) \
     AND ($13::text IS NULL OR auth_kind = $13)";

/// How many values `LIST_WHERE_SQL` binds; the next placeholder is `$14`.
const LIST_FILTER_BINDS: usize = 13;

/// Rows that still hold an account: what select-all, the quota summary and the batch
/// actions work on.
const HOLDS_ACCOUNT_SQL: &str = "status IN ('stored', 'invalid')";

type PgQuery<'q> = sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>;

/// The values bound to `LIST_WHERE_SQL`, shared by every query over the filtered rows so the
/// list, its counts, the export, select-all and the summary always agree.
struct ListBinds {
    status: Option<String>,
    pattern: Option<String>,
    id_query: Option<i64>,
    group: Option<String>,
    ungrouped: bool,
    origin_reason: Option<String>,
    from_pool: bool,
    imported: bool,
    subscription_titles: Option<Vec<String>>,
    quota: Option<String>,
    proxy: Option<String>,
    proxy_resource_id: Option<i64>,
    auth_kind: Option<String>,
}

impl ListBinds {
    fn new(filter: &InventoryListFilter) -> Self {
        let (group, ungrouped) = GroupFilter::binds(filter.group.as_ref());
        let (origin_reason, from_pool, imported) = OriginFilter::binds(filter.origin.as_ref());
        Self {
            status: filter.status.clone(),
            pattern: search_pattern(filter.query.as_deref()),
            id_query: search_id(filter.query.as_deref()),
            group: group.map(str::to_string),
            ungrouped,
            origin_reason: origin_reason.map(str::to_string),
            from_pool,
            imported,
            subscription_titles: filter.subscription_titles.clone(),
            quota: filter.quota.clone(),
            proxy: filter.proxy.clone(),
            proxy_resource_id: filter.proxy_resource_id.map(|id| id as i64),
            auth_kind: filter.auth_kind.clone(),
        }
    }

    /// The same filter without the status, for the status counts.
    fn any_status(mut self) -> Self {
        self.status = None;
        self
    }

    fn bind<'q>(&'q self, query: PgQuery<'q>) -> PgQuery<'q> {
        query
            .bind(self.status.as_deref())
            .bind(self.pattern.as_deref())
            .bind(self.id_query)
            .bind(self.group.as_deref())
            .bind(self.ungrouped)
            .bind(self.origin_reason.as_deref())
            .bind(self.from_pool)
            .bind(self.imported)
            .bind(self.subscription_titles.as_deref())
            .bind(self.quota.as_deref())
            .bind(self.proxy.as_deref())
            .bind(self.proxy_resource_id)
            .bind(self.auth_kind.as_deref())
    }
}

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
        let binds = ListBinds::new(filter);
        let count_sql = format!("SELECT count(*) FROM credential_inventory WHERE {LIST_WHERE_SQL}");
        let total: i64 = binds
            .bind(sqlx::query(&count_sql))
            .fetch_one(&self.pool)
            .await?
            .try_get(0)?;
        let list_sql = format!(
            "SELECT {ROW_COLUMNS} FROM credential_inventory WHERE {LIST_WHERE_SQL} \
             ORDER BY {} LIMIT ${} OFFSET ${}",
            filter.sort.order_by(filter.ascending),
            LIST_FILTER_BINDS + 1,
            LIST_FILTER_BINDS + 2,
        );
        let rows = binds
            .bind(sqlx::query(&list_sql))
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

    /// Status counts of the filtered rows, whatever status the filter selects.
    pub async fn filtered_counts(
        &self,
        filter: &InventoryListFilter,
    ) -> anyhow::Result<InventoryCounts> {
        let binds = ListBinds::new(filter).any_status();
        let sql = format!(
            "SELECT status, count(*) AS n FROM credential_inventory WHERE {LIST_WHERE_SQL} \
             GROUP BY status"
        );
        let rows = binds.bind(sqlx::query(&sql)).fetch_all(&self.pool).await?;
        let mut counts = InventoryCounts::default();
        for row in rows {
            let status: String = row.try_get("status")?;
            let n = row.try_get::<i64, _>("n")?.max(0) as u64;
            counts.add(&status, n);
        }
        Ok(counts)
    }

    /// Ids of the filtered rows that hold an account, in list order, at most `cap` of them,
    /// with how many there are.
    pub async fn filtered_ids(
        &self,
        filter: &InventoryListFilter,
        cap: usize,
    ) -> anyhow::Result<(Vec<u64>, u64)> {
        let binds = ListBinds::new(filter);
        let count_sql = format!(
            "SELECT count(*) FROM credential_inventory WHERE {LIST_WHERE_SQL} AND {HOLDS_ACCOUNT_SQL}"
        );
        let total: i64 = binds
            .bind(sqlx::query(&count_sql))
            .fetch_one(&self.pool)
            .await?
            .try_get(0)?;
        let ids_sql = format!(
            "SELECT id FROM credential_inventory WHERE {LIST_WHERE_SQL} AND {HOLDS_ACCOUNT_SQL} \
             ORDER BY {} LIMIT ${}",
            filter.sort.order_by(filter.ascending),
            LIST_FILTER_BINDS + 1,
        );
        let rows = binds
            .bind(sqlx::query(&ids_sql))
            .bind(cap as i64)
            .fetch_all(&self.pool)
            .await?;
        let ids = rows
            .into_iter()
            .map(|row| row.try_get::<i64, _>(0).map(|id| id as u64))
            .collect::<Result<_, _>>()?;
        Ok((ids, total.max(0) as u64))
    }

    /// Quota totals of the filtered rows that hold an account, per subscription title.
    pub async fn filtered_summary(
        &self,
        filter: &InventoryListFilter,
    ) -> anyhow::Result<Vec<InventoryTitleSummary>> {
        let binds = ListBinds::new(filter);
        let sql = format!(
            "SELECT COALESCE(account_info->>'subscriptionTitle', '') AS title, count(*) AS accounts, \
               count(*) FILTER (WHERE account_info IS NULL) AS unchecked, \
               count(*) FILTER (WHERE {REMAINING_SQL} <= 0) AS exhausted, \
               COALESCE(sum({REMAINING_SQL}) FILTER (WHERE {REMAINING_SQL} > 0), 0)::float8 AS remaining, \
               COALESCE(sum(CASE WHEN jsonb_typeof(account_info->'usageLimit') = 'number' \
                 THEN (account_info->>'usageLimit')::float8 END), 0)::float8 AS usage_limit \
             FROM credential_inventory WHERE {LIST_WHERE_SQL} AND {HOLDS_ACCOUNT_SQL} \
             GROUP BY 1 ORDER BY 2 DESC, 1"
        );
        let rows = binds.bind(sqlx::query(&sql)).fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|row| {
                Ok(InventoryTitleSummary {
                    title: row.try_get("title")?,
                    accounts: row.try_get::<i64, _>("accounts")?.max(0) as u64,
                    unchecked: row.try_get::<i64, _>("unchecked")?.max(0) as u64,
                    exhausted: row.try_get::<i64, _>("exhausted")?.max(0) as u64,
                    remaining: row.try_get("remaining")?,
                    usage_limit: row.try_get("usage_limit")?,
                })
            })
            .collect()
    }

    /// Every auth kind in the inventory, for the filter options.
    pub async fn auth_kinds(&self) -> anyhow::Result<Vec<String>> {
        Ok(sqlx::query_scalar(
            "SELECT DISTINCT auth_kind FROM credential_inventory WHERE deleted_at IS NULL ORDER BY 1",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    /// Every subscription title in the inventory, `""` for rows without one.
    pub async fn subscription_titles(&self) -> anyhow::Result<Vec<String>> {
        Ok(sqlx::query_scalar(
            "SELECT DISTINCT COALESCE(account_info->>'subscriptionTitle', '') \
             FROM credential_inventory WHERE deleted_at IS NULL",
        )
        .fetch_all(&self.pool)
        .await?)
    }

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
        let binds = ListBinds::new(filter);
        let sql = format!(
            "SELECT {ROW_COLUMNS} FROM credential_inventory WHERE {LIST_WHERE_SQL} ORDER BY id ASC"
        );
        let rows = binds.bind(sqlx::query(&sql)).fetch_all(&self.pool).await?;
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

    /// The keys `find_move_matches` skips a move on, for every live row not pushed out, so a
    /// preview can tell the duplicates without a query per account.
    pub async fn held_move_keys(&self) -> anyhow::Result<HeldMoveKeys> {
        let rows = sqlx::query(
            "SELECT refresh_token_hash, api_key_hash, identity_key, pushed_credential_id \
             FROM credential_inventory WHERE deleted_at IS NULL AND status <> 'pushed'",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut keys = HeldMoveKeys::default();
        for row in rows {
            if let Some(hash) = row.try_get::<Option<String>, _>("refresh_token_hash")? {
                keys.refresh_token_hashes.insert(hash);
            }
            if let Some(hash) = row.try_get::<Option<String>, _>("api_key_hash")? {
                keys.api_key_hashes.insert(hash);
            }
            if let Some(identity) = row.try_get::<Option<String>, _>("identity_key")? {
                keys.identity_keys.insert(identity);
            }
            if let Some(id) = row.try_get::<Option<i64>, _>("pushed_credential_id")? {
                keys.pushed_credential_ids.insert(id as u64);
            }
        }
        Ok(keys)
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
                source, group_name, account_info, last_check_at, origin_credential_id, origin_reason, \
                origin_snapshot) \
             VALUES ('pushing', $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) \
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
        .bind(moved.origin_snapshot)
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
               origin_reason = $12, origin_snapshot = $13, next_renew_at = NULL, updated_at = now() \
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
        .bind(moved.origin_snapshot)
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

    /// Refreshes one row under the lock the renewal takes (`FOR UPDATE SKIP LOCKED`), so two
    /// refreshes never spend the same refresh token and an edit, being an atomic patch, waits
    /// for the new token instead of writing back an old copy. The outcome is written in the
    /// same transaction. `Busy` when the renewal or another refresh holds the row.
    pub async fn refresh_locked<F, Fut>(&self, id: u64, refresh: F) -> anyhow::Result<LockedRefresh>
    where
        F: FnOnce(InventoryRow) -> Fut,
        Fut: std::future::Future<Output = RenewOutcome>,
    {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(&format!(
            "SELECT {ROW_COLUMNS} FROM credential_inventory \
             WHERE id = $1 AND {REFRESHABLE_SQL} FOR UPDATE SKIP LOCKED"
        ))
        .bind(id as i64)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            tx.rollback().await?;
            let refreshable: bool = sqlx::query_scalar(&format!(
                "SELECT EXISTS (SELECT 1 FROM credential_inventory WHERE id = $1 AND {REFRESHABLE_SQL})"
            ))
            .bind(id as i64)
            .fetch_one(&self.pool)
            .await?;
            return Ok(if refreshable {
                LockedRefresh::Busy
            } else {
                LockedRefresh::Missing
            });
        };
        let outcome = refresh(row_from_pg(row)?).await;
        if matches!(outcome, RenewOutcome::Deferred) {
            tx.rollback().await?;
            return Ok(LockedRefresh::Deferred);
        }
        write_refresh_outcome(&mut tx, id, &outcome).await?;
        tx.commit().await?;
        Ok(match outcome {
            RenewOutcome::Refreshed { data, .. } => LockedRefresh::Refreshed(data),
            RenewOutcome::Failed { error, invalid } => LockedRefresh::Failed { error, invalid },
            RenewOutcome::Deferred => LockedRefresh::Deferred,
        })
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

    /// Edits a stored row in place: drops `remove` keys from its data, merges `patch` over it,
    /// and sets the note when `note` is given (`Some(None)` clears it). One statement on the
    /// current value, so a refresh holding the row is waited for and its new token kept.
    pub async fn patch_data(
        &self,
        id: u64,
        remove: &[&str],
        patch: &serde_json::Value,
        note: Option<Option<&str>>,
    ) -> anyhow::Result<bool> {
        let remove: Vec<String> = remove.iter().map(|key| key.to_string()).collect();
        let result = sqlx::query(&format!(
            "UPDATE credential_inventory SET data = (data - $2::text[]) || $3::jsonb, \
               note = CASE WHEN $4 THEN $5 ELSE note END, updated_at = now() \
             WHERE id = $1 AND {REFRESHABLE_SQL}"
        ))
        .bind(id as i64)
        .bind(&remove)
        .bind(patch)
        .bind(note.is_some())
        .bind(note.flatten())
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
    ///
    /// A row is due when its access token expires before `due_before`, it was not renewed after
    /// `renewed_before`, and a failed renewal's backoff has passed. A `Deferred` outcome (the
    /// shared refresh limiter is busy) ends the pass without touching the row.
    pub async fn renew_due<F, Fut>(
        &self,
        due_before: DateTime<Utc>,
        renewed_before: DateTime<Utc>,
        max_rows: usize,
        mut renew: F,
    ) -> anyhow::Result<RenewPass>
    where
        F: FnMut(InventoryRow) -> Fut,
        Fut: std::future::Future<Output = RenewOutcome>,
    {
        let mut pass = RenewPass::default();
        let mut last_id: i64 = 0;
        for _ in 0..max_rows {
            let mut tx = self.pool.begin().await?;
            let row = sqlx::query(&format!(
                "SELECT {ROW_COLUMNS} FROM credential_inventory \
                 WHERE deleted_at IS NULL AND status = 'stored' AND auth_kind <> 'api_key' \
                   AND data IS NOT NULL AND id > $1 \
                   AND (NULLIF(data->>'expiresAt', '') IS NULL \
                        OR (data->>'expiresAt')::timestamptz <= $2) \
                   AND (last_refresh_at IS NULL OR last_refresh_at <= $3) \
                   AND (next_renew_at IS NULL OR next_renew_at <= now()) \
                 ORDER BY id ASC LIMIT 1 FOR UPDATE SKIP LOCKED"
            ))
            .bind(last_id)
            .bind(due_before)
            .bind(renewed_before)
            .fetch_optional(&mut *tx)
            .await?;
            let Some(row) = row else {
                tx.rollback().await?;
                break;
            };
            let row = row_from_pg(row)?;
            last_id = row.id as i64;
            let id = row.id;
            let outcome = renew(row).await;
            match &outcome {
                RenewOutcome::Refreshed { .. } => pass.renewed += 1,
                RenewOutcome::Failed { .. } => pass.failed += 1,
                RenewOutcome::Deferred => {
                    tx.rollback().await?;
                    pass.deferred = true;
                    break;
                }
            }
            write_refresh_outcome(&mut tx, id, &outcome).await?;
            tx.commit().await?;
        }
        Ok(pass)
    }
}

/// A row a refresh or an edit may touch: live, not pushed out, with its credential.
const REFRESHABLE_SQL: &str =
    "deleted_at IS NULL AND status IN ('stored', 'invalid') AND data IS NOT NULL";

/// Writes a refresh outcome in the transaction holding the row lock. A refreshed row is
/// `stored` again (a manual refresh recovers an `invalid` one); a failure backs off and marks
/// a rejected refresh token `invalid`.
async fn write_refresh_outcome(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: u64,
    outcome: &RenewOutcome,
) -> anyhow::Result<()> {
    match outcome {
        RenewOutcome::Refreshed {
            data,
            refresh_token_hash,
        } => {
            sqlx::query(
                "UPDATE credential_inventory SET data = $2, \
                   refresh_token_hash = COALESCE($3, refresh_token_hash), status = 'stored', \
                   last_refresh_at = now(), last_refresh_error = NULL, refresh_failure_count = 0, \
                   next_renew_at = NULL, updated_at = now() \
                 WHERE id = $1",
            )
            .bind(id as i64)
            .bind(data)
            .bind(refresh_token_hash.as_deref())
            .execute(&mut **tx)
            .await?;
        }
        RenewOutcome::Failed { error, invalid } => {
            sqlx::query(&format!(
                "UPDATE credential_inventory SET last_refresh_error = $2, \
                   refresh_failure_count = refresh_failure_count + 1, \
                   next_renew_at = {RENEW_BACKOFF_SQL}, \
                   status = CASE WHEN $3 THEN 'invalid' ELSE status END, updated_at = now() \
                 WHERE id = $1"
            ))
            .bind(id as i64)
            .bind(error)
            .bind(*invalid)
            .execute(&mut **tx)
            .await?;
        }
        RenewOutcome::Deferred => {}
    }
    Ok(())
}

/// Backoff after a failed renewal, from the failure count before this failure: 5 minutes,
/// doubling, at most a day. Read in an UPDATE, where columns still hold their old values.
const RENEW_BACKOFF_SQL: &str =
    "now() + make_interval(secs => LEAST(300 * power(2, LEAST(refresh_failure_count, 9)), 86400))";

/// What one renewal pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenewPass {
    pub renewed: usize,
    pub failed: usize,
    /// The shared refresh limiter was busy; the pass stopped early.
    pub deferred: bool,
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
    /// Not attempted: the refresh limiter shared with the pool is busy. Not a failure.
    Deferred,
}

/// What a manual refresh of one row did.
#[derive(Debug, Clone, PartialEq)]
pub enum LockedRefresh {
    /// The new token is stored and the row is `stored`; carries the stored data.
    Refreshed(serde_json::Value),
    Failed {
        error: String,
        invalid: bool,
    },
    /// The refresh limiter shared with the pool is busy; nothing changed.
    Deferred,
    /// The renewal or another refresh is refreshing this row right now.
    Busy,
    /// Not in the inventory, or pushed out.
    Missing,
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
            origin_snapshot: None,
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
            .filtered_counts(&InventoryListFilter {
                group: Some(GroupFilter::Named("甲".into())),
                ..InventoryListFilter::default()
            })
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

    #[tokio::test]
    async fn renewal_waits_for_the_interval_backs_off_and_yields() {
        let Some((postgres, store)) = test_store().await else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let expired = json!({"refreshToken": "r", "expiresAt": "2020-01-01T00:00:00Z"});
        let a = store
            .insert(new_row("h-renew-a", &expired, None))
            .await
            .unwrap();
        let b = store
            .insert(new_row("h-renew-b", &expired, None))
            .await
            .unwrap();
        let c = store
            .insert(new_row("h-renew-c", &expired, None))
            .await
            .unwrap();
        let now = Utc::now();
        let renewed = |data: &serde_json::Value| RenewOutcome::Refreshed {
            data: data.clone(),
            refresh_token_hash: None,
        };

        // a renews, b fails, c is never tried: the limiter is busy.
        let mut seen = Vec::new();
        let pass = store
            .renew_due(now, now, 10, |row| {
                seen.push(row.id);
                let outcome = if row.id == a.id {
                    renewed(&expired)
                } else if row.id == b.id {
                    RenewOutcome::Failed {
                        error: "boom".into(),
                        invalid: false,
                    }
                } else {
                    RenewOutcome::Deferred
                };
                async move { outcome }
            })
            .await
            .unwrap();
        assert_eq!(
            pass,
            RenewPass {
                renewed: 1,
                failed: 1,
                deferred: true
            }
        );
        assert_eq!(seen, vec![a.id, b.id, c.id]);
        let c_row = store.get(c.id).await.unwrap().unwrap();
        assert_eq!(
            c_row.refresh_failure_count, 0,
            "a deferred row is not a failure"
        );
        assert!(c_row.last_refresh_error.is_none());
        let b_row = store.get(b.id).await.unwrap().unwrap();
        assert_eq!(b_row.refresh_failure_count, 1);
        assert!(
            b_row.next_renew_at.unwrap() > Utc::now(),
            "a failure backs off"
        );

        // Next pass: a was renewed after `renewed_before`, b is backing off; only c is due.
        let mut seen = Vec::new();
        let pass = store
            .renew_due(now, now, 10, |row| {
                seen.push(row.id);
                let outcome = renewed(&expired);
                async move { outcome }
            })
            .await
            .unwrap();
        assert_eq!(seen, vec![c.id]);
        assert_eq!(pass.renewed, 1);
        // The pass size caps how many rows one pass renews.
        let pass = store
            .renew_due(
                now,
                Utc::now() + chrono::Duration::hours(1),
                1,
                |_| async move {
                    RenewOutcome::Failed {
                        error: "x".into(),
                        invalid: false,
                    }
                },
            )
            .await
            .unwrap();
        assert_eq!(pass.failed, 1);
        postgres.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn an_edit_waits_for_a_refresh_and_keeps_its_new_token() {
        let Some((postgres, store)) = test_store().await else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let data = json!({
            "refreshToken": "old",
            "authRegion": "us-east-1",
            "proxyUrl": "http://proxy.invalid:1",
            "proxyPassword": "secret",
        });
        let id = store
            .insert(new_row("h-edit", &data, None))
            .await
            .unwrap()
            .id;

        // A refresh holds the row while the upstream rotates the refresh token.
        let (started_tx, started) = tokio::sync::oneshot::channel::<()>();
        let (release, release_rx) = tokio::sync::oneshot::channel::<()>();
        let refreshing = {
            let store = store.clone();
            tokio::spawn(async move {
                store
                    .refresh_locked(id, |row| async move {
                        let _ = started_tx.send(());
                        let _ = release_rx.await;
                        let mut data = row.data.unwrap();
                        data["refreshToken"] = json!("new");
                        RenewOutcome::Refreshed {
                            data,
                            refresh_token_hash: Some("h-new".to_string()),
                        }
                    })
                    .await
            })
        };
        started.await.unwrap();
        // Another refresh finds the row busy and does not run.
        let busy = store
            .refresh_locked(id, |_| async { RenewOutcome::Deferred })
            .await
            .unwrap();
        assert_eq!(busy, LockedRefresh::Busy);
        // An edit sent meanwhile waits for the lock, then applies on top of the new token.
        let edit = {
            let store = store.clone();
            tokio::spawn(async move {
                store
                    .patch_data(
                        id,
                        &["proxyUrl", "proxyPassword"],
                        &json!({"authRegion": "eu-west-1"}),
                        Some(Some("备注")),
                    )
                    .await
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(!edit.is_finished(), "the edit waits for the refresh");
        release.send(()).unwrap();
        assert!(matches!(
            refreshing.await.unwrap().unwrap(),
            LockedRefresh::Refreshed(_)
        ));
        assert!(edit.await.unwrap().unwrap());

        let row = store.get(id).await.unwrap().unwrap();
        let data = row.data.clone().unwrap();
        assert_eq!(
            data["refreshToken"], "new",
            "the rotated token survives the edit"
        );
        assert_eq!(data["authRegion"], "eu-west-1");
        assert!(data.get("proxyUrl").is_none() && data.get("proxyPassword").is_none());
        assert_eq!(row.refresh_token_hash.as_deref(), Some("h-new"));
        assert_eq!(row.note.as_deref(), Some("备注"));

        // The note can be cleared; a missing note leaves it alone.
        assert!(
            store
                .patch_data(id, &[], &json!({}), Some(None))
                .await
                .unwrap()
        );
        assert_eq!(store.get(id).await.unwrap().unwrap().note, None);
        postgres.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn a_manual_refresh_marks_recovers_and_yields() {
        let Some((postgres, store)) = test_store().await else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let data = json!({"refreshToken": "r"});
        let id = store
            .insert(new_row("h-manual", &data, None))
            .await
            .unwrap()
            .id;

        let rejected = store
            .refresh_locked(id, |_| async {
                RenewOutcome::Failed {
                    error: "invalid_grant".to_string(),
                    invalid: true,
                }
            })
            .await
            .unwrap();
        assert!(matches!(
            rejected,
            LockedRefresh::Failed { invalid: true, .. }
        ));
        let row = store.get(id).await.unwrap().unwrap();
        assert_eq!(row.status, STATUS_INVALID);
        assert_eq!(row.refresh_failure_count, 1);
        assert!(row.next_renew_at.is_some());

        // The shared limiter said no: nothing changes.
        let deferred = store
            .refresh_locked(id, |_| async { RenewOutcome::Deferred })
            .await
            .unwrap();
        assert_eq!(deferred, LockedRefresh::Deferred);
        assert_eq!(
            store.get(id).await.unwrap().unwrap().refresh_failure_count,
            1
        );

        // A successful manual refresh brings an invalid row back.
        let refreshed = store
            .refresh_locked(id, |row| async move {
                RenewOutcome::Refreshed {
                    data: row.data.unwrap(),
                    refresh_token_hash: None,
                }
            })
            .await
            .unwrap();
        assert!(matches!(refreshed, LockedRefresh::Refreshed(_)));
        let row = store.get(id).await.unwrap().unwrap();
        assert_eq!(row.status, STATUS_STORED);
        assert_eq!(row.refresh_failure_count, 0);
        assert!(row.next_renew_at.is_none() && row.last_refresh_at.is_some());
        assert_eq!(row.refresh_token_hash.as_deref(), Some("h-manual"));

        let missing = store
            .refresh_locked(id + 1000, |_| async { RenewOutcome::Deferred })
            .await
            .unwrap();
        assert_eq!(missing, LockedRefresh::Missing);
        postgres.drop_test_schema().await.unwrap();
    }

    #[test]
    fn the_filter_binds_every_placeholder() {
        let highest = LIST_WHERE_SQL
            .split('$')
            .skip(1)
            .filter_map(|rest| {
                rest.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse::<usize>()
                    .ok()
            })
            .max();
        assert_eq!(highest, Some(LIST_FILTER_BINDS));
    }

    #[test]
    fn sorts_are_whitelisted_and_stable() {
        assert_eq!(
            InventorySort::parse(Some("id; drop table")),
            InventorySort::Newest
        );
        assert_eq!(InventorySort::Newest.order_by(true), "id DESC");
        for sort in ["remaining", "checked", "created", "expires"] {
            let order = InventorySort::parse(Some(sort)).order_by(true);
            assert!(order.ends_with("ASC NULLS LAST, id DESC"), "{order}");
        }
    }

    #[tokio::test]
    async fn filters_sort_select_all_and_summary_agree() {
        let Some((postgres, store)) = test_store().await else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let with_resource = json!({"refreshToken": "a", "proxyResourceId": 3});
        let with_url = json!({"refreshToken": "b", "proxyUrl": "http://proxy.invalid:1"});
        let bare = json!({"refreshToken": "c"});
        let a = store
            .insert(new_row("h-fa", &with_resource, None))
            .await
            .unwrap()
            .id;
        let b = store
            .insert(NewInventoryRow {
                auth_kind: "idc",
                ..new_row("h-fb", &with_url, None)
            })
            .await
            .unwrap()
            .id;
        let c = store.insert(new_row("h-fc", &bare, None)).await.unwrap().id;
        let pro = json!({"subscriptionTitle": "KIRO PRO", "remaining": 50.0, "usageLimit": 100.0});
        let free = json!({"subscriptionTitle": "KIRO FREE", "remaining": 0.0, "usageLimit": 50.0});
        store.save_check(a, Some(&pro), None, None).await.unwrap();
        store.save_check(b, Some(&free), None, None).await.unwrap();

        let listed = |filter: InventoryListFilter| {
            let store = store.clone();
            async move {
                let filter = InventoryListFilter {
                    limit: 100,
                    ..filter
                };
                let (rows, total) = store.list(&filter).await.unwrap();
                assert_eq!(total as usize, rows.len());
                rows.into_iter().map(|row| row.id).collect::<Vec<_>>()
            }
        };
        let quota = |value: &str| InventoryListFilter {
            quota: Some(value.to_string()),
            ..InventoryListFilter::default()
        };
        assert_eq!(listed(quota("remaining")).await, vec![a]);
        assert_eq!(listed(quota("exhausted")).await, vec![b]);
        assert_eq!(listed(quota("unchecked")).await, vec![c]);
        let proxy = |value: &str| InventoryListFilter {
            proxy: Some(value.to_string()),
            ..InventoryListFilter::default()
        };
        assert_eq!(listed(proxy("with")).await, vec![b, a]);
        assert_eq!(listed(proxy("without")).await, vec![c]);
        let resource = InventoryListFilter {
            proxy_resource_id: Some(3),
            ..InventoryListFilter::default()
        };
        assert_eq!(listed(resource).await, vec![a]);
        let titles = |titles: &[&str]| InventoryListFilter {
            subscription_titles: Some(titles.iter().map(|t| t.to_string()).collect()),
            ..InventoryListFilter::default()
        };
        assert_eq!(listed(titles(&["KIRO PRO"])).await, vec![a]);
        assert_eq!(listed(titles(&[""])).await, vec![c]);
        assert!(
            listed(titles(&[])).await.is_empty(),
            "no matching title matches nothing"
        );
        let auth = InventoryListFilter {
            auth_kind: Some("idc".to_string()),
            ..InventoryListFilter::default()
        };
        assert_eq!(listed(auth).await, vec![b]);

        // Empty values sort last in both directions.
        let by_remaining = |ascending| InventoryListFilter {
            sort: InventorySort::Remaining,
            ascending,
            ..InventoryListFilter::default()
        };
        assert_eq!(listed(by_remaining(false)).await, vec![a, b, c]);
        assert_eq!(listed(by_remaining(true)).await, vec![b, a, c]);

        // Select-all follows the list order, capped, with the full count.
        let (ids, total) = store.filtered_ids(&by_remaining(false), 2).await.unwrap();
        assert_eq!((ids, total), (vec![a, b], 3));

        let summary = store
            .filtered_summary(&InventoryListFilter::default())
            .await
            .unwrap();
        let of = |title: &str| {
            summary
                .iter()
                .find(|row| row.title == title)
                .cloned()
                .unwrap()
        };
        assert_eq!((of("").accounts, of("").unchecked), (1, 1));
        assert_eq!(
            (of("KIRO FREE").exhausted, of("KIRO FREE").usage_limit),
            (1, 50.0)
        );
        assert_eq!(
            (of("KIRO PRO").remaining, of("KIRO PRO").usage_limit),
            (50.0, 100.0)
        );

        // The status tabs count within the other filters, whatever status is selected.
        let counts = store
            .filtered_counts(&InventoryListFilter {
                status: Some(STATUS_PUSHED.to_string()),
                proxy: Some("with".to_string()),
                ..InventoryListFilter::default()
            })
            .await
            .unwrap();
        assert_eq!((counts.total, counts.stored), (2, 2));
        let mut titles = store.subscription_titles().await.unwrap();
        titles.sort();
        assert_eq!(titles, vec!["", "KIRO FREE", "KIRO PRO"]);
        assert_eq!(store.auth_kinds().await.unwrap(), vec!["idc", "social"]);
        postgres.drop_test_schema().await.unwrap();
    }

    async fn origin_ids(store: &InventoryStore, filter: &str) -> Vec<u64> {
        store
            .list(&InventoryListFilter {
                origin: OriginFilter::parse(Some(filter)),
                limit: 100,
                ..InventoryListFilter::default()
            })
            .await
            .unwrap()
            .0
            .into_iter()
            .map(|row| row.id)
            .collect()
    }

    #[tokio::test]
    async fn rows_filter_by_where_they_came_from() {
        let Some((postgres, store)) = test_store().await else {
            eprintln!("跳过 PgSQL 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let data = json!({"refreshToken": "r"});
        let imported = store
            .insert(new_row("h-imported", &data, None))
            .await
            .unwrap();
        let snapshot = json!({"priority": 3, "healthStatus": "quota_exhausted"});
        let mut from_pool = moved("h-pool", &data, None, 8);
        from_pool.origin_snapshot = Some(&snapshot);
        let moved_row = store.begin_move_insert(&from_pool).await.unwrap();
        store.finish_move(moved_row.id, false).await.unwrap();
        assert_eq!(
            store
                .get(moved_row.id)
                .await
                .unwrap()
                .unwrap()
                .origin_snapshot,
            Some(snapshot)
        );
        assert_eq!(
            origin_ids(&store, OriginFilter::POOL).await,
            vec![moved_row.id]
        );
        assert_eq!(
            origin_ids(&store, OriginFilter::IMPORTED).await,
            vec![imported.id]
        );
        assert_eq!(
            origin_ids(&store, "QuotaExceeded").await,
            vec![moved_row.id]
        );
        assert!(origin_ids(&store, "Manual").await.is_empty());
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
