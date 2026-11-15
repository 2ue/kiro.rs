use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

// Kept outside credential JSON so generated keys never change scheduler authentication.
pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS credential_kiro_api_keys (
    credential_id BIGINT NOT NULL REFERENCES credentials(id) ON DELETE CASCADE,
    key_id TEXT NOT NULL,
    api_key TEXT NOT NULL,
    label TEXT NOT NULL,
    key_prefix TEXT NOT NULL,
    region TEXT NOT NULL,
    profile_arn TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    PRIMARY KEY (credential_id, key_id)
);
"#;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredKiroApiKey {
    pub credential_id: u64,
    pub key_id: String,
    pub api_key: String,
    pub label: String,
    pub key_prefix: String,
    pub region: String,
    pub profile_arn: String,
    pub created_at: i64,
}

pub async fn save(pool: &PgPool, key: &StoredKiroApiKey) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO credential_kiro_api_keys
         (credential_id, key_id, api_key, label, key_prefix, region, profile_arn, created_at)
         SELECT $1,$2,$3,$4,$5,$6,$7,$8 FROM credentials WHERE id=$1 AND deleted_at IS NULL AND lower(auth_kind)<>'api_key' AND api_key_hash IS NULL
         ON CONFLICT (credential_id, key_id) DO UPDATE SET api_key=EXCLUDED.api_key
         WHERE credential_kiro_api_keys.api_key=EXCLUDED.api_key
         RETURNING key_id",
    )
    .bind(i64::try_from(key.credential_id)?)
    .bind(&key.key_id)
    .bind(&key.api_key)
    .bind(&key.label)
    .bind(&key.key_prefix)
    .bind(&key.region)
    .bind(&key.profile_arn)
    .bind(key.created_at)
    .fetch_one(pool)
    .await?;
    Ok(())
}

pub async fn get(
    pool: &PgPool,
    credential_id: u64,
    key_id: &str,
) -> anyhow::Result<Option<StoredKiroApiKey>> {
    let row =
        sqlx::query("SELECT * FROM credential_kiro_api_keys WHERE credential_id=$1 AND key_id=$2")
            .bind(i64::try_from(credential_id)?)
            .bind(key_id)
            .fetch_optional(pool)
            .await?;
    row.map(key_from_row).transpose()
}

fn key_from_row(row: sqlx::postgres::PgRow) -> anyhow::Result<StoredKiroApiKey> {
    Ok(StoredKiroApiKey {
        credential_id: u64::try_from(row.try_get::<i64, _>("credential_id")?)?,
        key_id: row.try_get("key_id")?,
        api_key: row.try_get("api_key")?,
        label: row.try_get("label")?,
        key_prefix: row.try_get("key_prefix")?,
        region: row.try_get("region")?,
        profile_arn: row.try_get("profile_arn")?,
        created_at: row.try_get("created_at")?,
    })
}

pub async fn check_available(pool: &PgPool) -> anyhow::Result<()> {
    sqlx::query("SELECT key_id FROM credential_kiro_api_keys LIMIT 0")
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn list(
    pool: &PgPool,
    ids: &[u64],
    offset: i64,
) -> anyhow::Result<Vec<StoredKiroApiKey>> {
    let ids = ids
        .iter()
        .map(|id| i64::try_from(*id))
        .collect::<Result<Vec<_>, _>>()?;
    let rows = sqlx::query(
        "SELECT k.* FROM credential_kiro_api_keys k
         JOIN credentials c ON c.id=k.credential_id AND c.deleted_at IS NULL AND lower(c.auth_kind)<>'api_key' AND c.api_key_hash IS NULL
         WHERE k.credential_id=ANY($1) ORDER BY k.created_at DESC,k.credential_id,k.key_id
         LIMIT 500 OFFSET $2",
    )
    .bind(ids)
    .bind(offset.max(0))
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(key_from_row).collect()
}

pub async fn remove(pool: &PgPool, credential_id: u64, key_id: &str) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM credential_kiro_api_keys WHERE credential_id=$1 AND key_id=$2")
        .bind(i64::try_from(credential_id)?)
        .bind(key_id)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model::config::Config, storage::postgres::PostgresStore};

    #[tokio::test]
    async fn official_kiro_key_storage_survives_peer_reads_and_preserves_source_auth() {
        let Some(url) = crate::storage::integration_test_url("KIRO_RS_TEST_POSTGRES_URL") else {
            return;
        };
        let mut config = Config::default();
        config.postgres.url = Some(url);
        let store = PostgresStore::connect_test(&config).await.unwrap();
        let pool = store.pool();
        for (id, auth) in [
            (1i64, "idc"),
            (2, "social"),
            (3, "external_idp"),
            (4, "oauth"),
            (5, "api_key"),
        ] {
            sqlx::query("INSERT INTO credentials(id,data,auth_kind) VALUES($1,$2,$3)")
                .bind(id)
                .bind(serde_json::json!({"authMethod":auth,"refreshToken":"original-refresh"}))
                .bind(auth)
                .execute(pool)
                .await
                .unwrap();
        }
        assert!(
            list(pool, &[1, 2], 0).await.unwrap().is_empty(),
            "no historical backfill"
        );
        let key = StoredKiroApiKey {
            credential_id: 1,
            key_id: "official-id".into(),
            api_key: "ksk_test_complete".into(),
            label: "name".into(),
            key_prefix: "ksk_test".into(),
            region: "us-east-1".into(),
            profile_arn: "profile".into(),
            created_at: 1786856404260,
        };
        save(pool, &key).await.unwrap();
        save(pool, &key).await.unwrap();
        let peer = PostgresStore::connect_test_peer(&config, &store)
            .await
            .unwrap();
        assert_eq!(
            list(peer.pool(), &[1], 0).await.unwrap()[0].api_key,
            key.api_key
        );
        assert!(list(pool, &[2], 0).await.unwrap().is_empty());
        assert!(get(pool, 2, &key.key_id).await.unwrap().is_none());
        let row = sqlx::query("SELECT data,auth_kind,api_key_hash FROM credentials WHERE id=1")
            .fetch_one(pool)
            .await
            .unwrap();
        let data: serde_json::Value = row.get("data");
        assert_eq!(data["authMethod"], "idc");
        assert_eq!(data["refreshToken"], "original-refresh");
        assert!(data.get("kiroApiKey").is_none());
        assert_eq!(row.get::<String, _>("auth_kind"), "idc");
        assert!(row.get::<Option<String>, _>("api_key_hash").is_none());
        for id in [2, 3, 4] {
            let mut other = key.clone();
            other.credential_id = id;
            save(pool, &other).await.unwrap();
            assert_eq!(list(pool, &[id], 0).await.unwrap().len(), 1);
            remove(pool, id, &other.key_id).await.unwrap();
        }
        let mut imported_key = key.clone();
        imported_key.credential_id = 5;
        assert!(save(pool, &imported_key).await.is_err());
        let mut conflict = key.clone();
        conflict.api_key = "ksk_other_secret".into();
        assert!(save(pool, &conflict).await.is_err());
        remove(pool, 2, &key.key_id).await.unwrap();
        assert!(get(pool, 1, &key.key_id).await.unwrap().is_some());
        remove(pool, 1, &key.key_id).await.unwrap();
        remove(pool, 1, &key.key_id).await.unwrap();
        assert!(list(pool, &[1], 0).await.unwrap().is_empty());
        peer.pool().close().await;
        store.drop_test_schema().await.unwrap();
    }
}
