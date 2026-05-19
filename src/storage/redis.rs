use anyhow::Context;
use bb8::Pool;
use bb8_redis::RedisConnectionManager;

pub type RedisPool = Pool<RedisConnectionManager>;

/// 建立 Redis 连接池并 ping 一次确认可用。
pub async fn redis_pool(redis_url: &str) -> anyhow::Result<RedisPool> {
    let manager = RedisConnectionManager::new(redis_url)
        .with_context(|| format!("解析 REDIS_URL 失败: {}", redis_url))?;

    let pool = Pool::builder()
        .max_size(20)
        .min_idle(Some(1))
        .build(manager)
        .await
        .context("建立 Redis 连接池失败")?;

    {
        let mut conn = pool.get().await.context("从 Redis 连接池获取连接失败")?;
        let pong: String = ::redis::cmd("PING")
            .query_async(&mut *conn)
            .await
            .context("Redis PING 失败")?;
        if pong != "PONG" {
            anyhow::bail!("Redis PING 返回非预期值: {}", pong);
        }
    }

    Ok(pool)
}
