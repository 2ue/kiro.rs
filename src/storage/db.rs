use anyhow::Context;
use sqlx::ConnectOptions;
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;
use std::time::Duration;

pub type Db = PgPool;

/// 建立 PG 连接池并 ping 一次确认可用。
pub async fn connect(database_url: &str, acquire_timeout: Duration) -> anyhow::Result<Db> {
    let mut opts = PgConnectOptions::from_str(database_url)
        .with_context(|| format!("解析 DATABASE_URL 失败: {}", database_url))?;
    opts = opts.log_statements(tracing::log::LevelFilter::Debug);

    let pool = PgPoolOptions::new()
        .max_connections(20)
        .min_connections(1)
        .acquire_timeout(acquire_timeout)
        .test_before_acquire(true)
        .connect_with(opts)
        .await
        .context("建立 PG 连接池失败")?;

    sqlx::query("SELECT 1")
        .execute(&pool)
        .await
        .context("PG 连接 ping 失败")?;
    Ok(pool)
}

/// 执行 `migrations/` 目录下所有 SQL 迁移。
pub async fn run_migrations(pool: &Db) -> anyhow::Result<()> {
    sqlx::migrate!("./migrations")
        .run(pool)
        .await
        .context("sqlx::migrate! 失败")?;
    Ok(())
}
