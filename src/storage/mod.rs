//! 数据持久化层
//!
//! - PostgreSQL: 凭据 / 用量 / 计价 / 配置 / 配额事件 等权威数据
//! - Redis: 余额缓存 / sticky session / 计价 JSON 缓存 / 冷却窗口 等可重建状态
//!
//! 启动时严格依赖两者均可用,任一连接失败即 panic 退出(参见 [`init`])。

mod db;
mod redis;

pub use db::{Db, run_migrations};
pub use redis::RedisPool;

use anyhow::Context;
use std::time::Duration;

/// 全部存储依赖句柄
#[derive(Clone)]
pub struct Storage {
    pub db: Db,
    pub redis: RedisPool,
}

/// 启动期初始化:连接 PG / Redis,执行 schema migrations,健康检查通过才返回。
///
/// `database_url` / `redis_url` 由 [`crate::model::config::Config`] 在启动时读取,
/// 通常来自 `config.json` 的 `databaseUrl` / `redisUrl` 字段或同名环境变量。
pub async fn init(database_url: &str, redis_url: &str) -> anyhow::Result<Storage> {
    tracing::info!("正在连接 PostgreSQL...");
    let db = db::connect(database_url, Duration::from_secs(10))
        .await
        .context("连接 PostgreSQL 失败")?;

    tracing::info!("正在执行数据库迁移...");
    run_migrations(&db).await.context("执行数据库迁移失败")?;

    tracing::info!("正在连接 Redis...");
    let redis = redis::redis_pool(redis_url)
        .await
        .context("连接 Redis 失败")?;

    tracing::info!("数据持久化层初始化完成");
    Ok(Storage { db, redis })
}
