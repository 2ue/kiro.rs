//! 在线运行时配置(app_config 表)
//!
//! 对应 migration 0004。静态启动项(host/port/databaseUrl/redisUrl/api_key/admin_api_key)仍在
//! `config.json`,运行时可改的项(loadBalancingMode、缓存模拟、配额阈值等)落库,
//! 修改即时生效。
//!
//! 注意:本服务**只**负责 KV 读写。具体业务模块对配置的"热加载"由 watcher / 直接读取实现。

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;

use crate::storage::Db;

/// 当前已知的运行时配置 key 白名单。
///
/// 写入时若 key 不在此列表,会拒绝(避免拼写错误污染表)。
pub const KNOWN_KEYS: &[&str] = &[
    "load_balancing_mode",
    "compat_profile",
    "extract_thinking",
    "prompt_cache_simulation_mode",
    "prompt_cache_target_read_ratio",
    "prompt_cache_token_scale",
    "prompt_cache_max_simulated_input_tokens",
    "prompt_cache_cap_jitter_min_tokens",
    "prompt_cache_cap_jitter_max_tokens",
    "prompt_cache_scale_min_input_tokens",
    "prompt_cache_creation_ratio_min",
    "prompt_cache_creation_ratio_max",
    "prompt_cache_creation_burst_probability",
    "prompt_cache_min_cacheable_tokens",
    "high_cache_threshold",
    "default_endpoint",
    "expose_proxy_warnings",
    "quota_soft_fail_limit",
    "quota_cooldown_minutes",
    "pricing_auto_sync_enabled",
    "pricing_source_url",
    "pricing_bootstrap_done",
    "balance_cache_ttl_seconds",
    "session_binding_ttl_minutes",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigEntry {
    pub key: String,
    pub value: Value,
    pub description: Option<String>,
    pub updated_by: String,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// 简单内存缓存,带 RwLock。每次写入后失效,读时按需 refill。
pub struct AppConfigService {
    db: Db,
    cache: RwLock<HashMap<String, Value>>,
}

impl AppConfigService {
    pub async fn new(db: Db) -> anyhow::Result<Arc<Self>> {
        let svc = Arc::new(Self {
            db,
            cache: RwLock::new(HashMap::new()),
        });
        svc.refresh_cache().await?;
        Ok(svc)
    }

    /// 全量刷新缓存(启动时 / 手动同步)
    pub async fn refresh_cache(&self) -> anyhow::Result<()> {
        let rows = sqlx::query("SELECT key, value FROM app_config")
            .fetch_all(&self.db)
            .await
            .context("加载 app_config 失败")?;
        let mut map = HashMap::with_capacity(rows.len());
        for row in rows {
            let k: String = row.try_get("key")?;
            let v: Value = row.try_get("value")?;
            map.insert(k, v);
        }
        *self.cache.write() = map;
        Ok(())
    }

    /// 读取(命中缓存)
    #[allow(dead_code)]
    pub fn get(&self, key: &str) -> Option<Value> {
        self.cache.read().get(key).cloned()
    }

    /// 读取并解析为指定类型
    pub fn get_as<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.cache
            .read()
            .get(key)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    /// 列出所有配置(用于设置页展示)
    pub async fn list(&self) -> anyhow::Result<Vec<ConfigEntry>> {
        let rows = sqlx::query(
            "SELECT key, value, description, updated_by, updated_at FROM app_config ORDER BY key",
        )
        .fetch_all(&self.db)
        .await
        .context("查询 app_config 列表失败")?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(ConfigEntry {
                key: row.try_get("key")?,
                value: row.try_get("value")?,
                description: row.try_get("description").ok(),
                updated_by: row.try_get("updated_by")?,
                updated_at: row.try_get("updated_at")?,
            });
        }
        Ok(out)
    }

    /// 写入单个 key(白名单校验)
    pub async fn set(&self, key: &str, value: Value, updated_by: &str) -> anyhow::Result<()> {
        if !KNOWN_KEYS.contains(&key) {
            anyhow::bail!("未知配置项: {}", key);
        }
        sqlx::query(
            "INSERT INTO app_config (key, value, updated_by) VALUES ($1, $2, $3) \
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_by = EXCLUDED.updated_by, updated_at = NOW()",
        )
        .bind(key)
        .bind(&value)
        .bind(updated_by)
        .execute(&self.db)
        .await
        .with_context(|| format!("写入 app_config[{}] 失败", key))?;

        self.cache.write().insert(key.to_string(), value);
        Ok(())
    }

    /// 批量写入(用于设置页"保存所有")
    pub async fn set_many(
        &self,
        items: &[(String, Value)],
        updated_by: &str,
    ) -> anyhow::Result<()> {
        for (k, _) in items {
            if !KNOWN_KEYS.contains(&k.as_str()) {
                anyhow::bail!("未知配置项: {}", k);
            }
        }
        let mut tx = self.db.begin().await?;
        for (k, v) in items {
            sqlx::query(
                "INSERT INTO app_config (key, value, updated_by) VALUES ($1, $2, $3) \
                 ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_by = EXCLUDED.updated_by, updated_at = NOW()",
            )
            .bind(k)
            .bind(v)
            .bind(updated_by)
            .execute(&mut *tx)
            .await
            .with_context(|| format!("写入 app_config[{}] 失败", k))?;
        }
        tx.commit().await?;

        let mut cache = self.cache.write();
        for (k, v) in items {
            cache.insert(k.clone(), v.clone());
        }
        Ok(())
    }
}
