use std::collections::HashMap;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Utc};
use redis::AsyncCommands;
use redis::aio::{ConnectionManager, PubSub};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::model::config::Config;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SchedulerSessionBinding {
    pub credential_id: u64,
    pub last_used_at: DateTime<Utc>,
    pub soft_failure_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SchedulerCooldownState {
    pub until_ms: i64,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerInFlightLease {
    pub id: u64,
    pub acquired_at_ms: i64,
    pub last_seen_at_ms: i64,
    pub kind: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SchedulerCredentialState {
    pub cooldown: Option<SchedulerCooldownState>,
    pub rate_limit_available_at_ms: Option<i64>,
    pub in_flight_leases: Vec<SchedulerInFlightLease>,
}

#[derive(Clone)]
pub struct RedisStore {
    client: redis::Client,
    manager: ConnectionManager,
    key_prefix: String,
}

impl RedisStore {
    pub async fn connect(config: &Config) -> anyhow::Result<Self> {
        let url = config
            .redis
            .url
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("必须配置 redis.url"))?;
        let client = redis::Client::open(url)?;
        let manager = client.get_connection_manager().await?;
        Ok(Self {
            client,
            manager,
            key_prefix: config.redis.key_prefix.trim_end_matches(':').to_string(),
        })
    }

    pub fn key(&self, suffix: impl AsRef<str>) -> String {
        format!(
            "{}:{}",
            self.key_prefix,
            suffix.as_ref().trim_start_matches(':')
        )
    }

    pub async fn ping(&self) -> anyhow::Result<()> {
        let mut manager = self.manager.clone();
        let _: String = redis::cmd("PING").query_async(&mut manager).await?;
        Ok(())
    }

    pub fn runtime_config_changed_channel(&self) -> String {
        self.key("events:runtime_config_changed")
    }

    pub fn credentials_changed_channel(&self) -> String {
        self.key("events:credentials_changed")
    }

    pub fn dispatch_wakeup_channel(&self) -> String {
        self.key("events:dispatch_wakeup")
    }

    pub async fn subscribe_runtime_events(&self) -> anyhow::Result<PubSub> {
        let mut pubsub = self.client.get_async_pubsub().await?;
        pubsub
            .subscribe(self.runtime_config_changed_channel())
            .await?;
        pubsub.subscribe(self.credentials_changed_channel()).await?;
        pubsub.subscribe(self.dispatch_wakeup_channel()).await?;
        Ok(pubsub)
    }

    async fn publish_event(&self, channel: String, payload: impl AsRef<str>) -> anyhow::Result<()> {
        let mut manager = self.manager.clone();
        let _: i64 = manager.publish(channel, payload.as_ref()).await?;
        Ok(())
    }

    pub async fn publish_runtime_config_changed(
        &self,
        payload: impl AsRef<str>,
    ) -> anyhow::Result<()> {
        self.publish_event(self.runtime_config_changed_channel(), payload)
            .await
    }

    pub async fn publish_credentials_changed(
        &self,
        payload: impl AsRef<str>,
    ) -> anyhow::Result<()> {
        self.publish_event(self.credentials_changed_channel(), payload)
            .await
    }

    pub async fn publish_dispatch_wakeup(&self, payload: impl AsRef<str>) -> anyhow::Result<()> {
        self.publish_event(self.dispatch_wakeup_channel(), payload)
            .await
    }

    pub async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        key: impl AsRef<str>,
    ) -> anyhow::Result<Option<T>> {
        let mut manager = self.manager.clone();
        let value: Option<String> = manager.get(self.key(key)).await?;
        let Some(value) = value else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_str(&value)?))
    }

    pub async fn set_json<T: serde::Serialize>(
        &self,
        key: impl AsRef<str>,
        value: &T,
        ttl_secs: usize,
    ) -> anyhow::Result<()> {
        let encoded = serde_json::to_string(value)?;
        let mut manager = self.manager.clone();
        let _: () = manager
            .set_ex(self.key(key), encoded, ttl_secs as u64)
            .await?;
        Ok(())
    }

    pub async fn del(&self, key: impl AsRef<str>) -> anyhow::Result<()> {
        let mut manager = self.manager.clone();
        let _: () = manager.del(self.key(key)).await?;
        Ok(())
    }

    pub async fn set_nx_ex(
        &self,
        key: impl AsRef<str>,
        value: impl AsRef<str>,
        ttl_secs: usize,
    ) -> anyhow::Result<bool> {
        let mut manager = self.manager.clone();
        let result: Option<String> = redis::cmd("SET")
            .arg(self.key(key))
            .arg(value.as_ref())
            .arg("NX")
            .arg("EX")
            .arg(ttl_secs.max(1))
            .query_async(&mut manager)
            .await?;
        Ok(result.as_deref() == Some("OK"))
    }

    pub async fn release_lock(
        &self,
        key: impl AsRef<str>,
        value: impl AsRef<str>,
    ) -> anyhow::Result<bool> {
        let script = r#"
            if redis.call('GET', KEYS[1]) == ARGV[1] then
                return redis.call('DEL', KEYS[1])
            end
            return 0
        "#;
        let mut manager = self.manager.clone();
        let removed: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(self.key(key))
            .arg(value.as_ref())
            .query_async(&mut manager)
            .await?;
        Ok(removed > 0)
    }

    pub async fn get_session_binding(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<SchedulerSessionBinding>> {
        self.get_json(session_binding_key(session_id)).await
    }

    pub async fn set_session_binding(
        &self,
        session_id: &str,
        binding: &SchedulerSessionBinding,
        ttl_secs: usize,
    ) -> anyhow::Result<()> {
        let encoded = serde_json::to_string(binding)?;
        let session_hash = session_hash(session_id);
        let script = r#"
            local old = redis.call('GET', KEYS[1])
            if old then
                local ok, parsed = pcall(cjson.decode, old)
                if ok and parsed['credential_id'] then
                    local old_id = tostring(parsed['credential_id'])
                    if old_id ~= ARGV[2] then
                        redis.call('SREM', ARGV[5] .. old_id, ARGV[1])
                    end
                end
            end
            redis.call('SET', KEYS[1], ARGV[3], 'EX', ARGV[4])
            redis.call('SADD', ARGV[5] .. ARGV[2], ARGV[1])
            redis.call('EXPIRE', ARGV[5] .. ARGV[2], ARGV[4])
            return 1
        "#;
        let mut manager = self.manager.clone();
        let _: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(self.key(session_binding_key(session_id)))
            .arg(&session_hash)
            .arg(binding.credential_id.to_string())
            .arg(encoded)
            .arg(ttl_secs.max(1))
            .arg(self.key("scheduler:sessions_by_credential:"))
            .query_async(&mut manager)
            .await?;
        Ok(())
    }

    pub async fn delete_session_binding(&self, session_id: &str) -> anyhow::Result<()> {
        let session_hash = session_hash(session_id);
        let script = r#"
            local old = redis.call('GET', KEYS[1])
            redis.call('DEL', KEYS[1])
            if old then
                local ok, parsed = pcall(cjson.decode, old)
                if ok and parsed['credential_id'] then
                    redis.call('SREM', ARGV[2] .. tostring(parsed['credential_id']), ARGV[1])
                end
            end
            return 1
        "#;
        let mut manager = self.manager.clone();
        let _: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(self.key(session_binding_key(session_id)))
            .arg(&session_hash)
            .arg(self.key("scheduler:sessions_by_credential:"))
            .query_async(&mut manager)
            .await?;
        Ok(())
    }

    pub async fn delete_sessions_for_credential(
        &self,
        credential_id: u64,
    ) -> anyhow::Result<usize> {
        let set_key = self.key(sessions_by_credential_key(credential_id));
        let mut manager = self.manager.clone();
        let session_hashes: Vec<String> = manager.smembers(&set_key).await?;
        let mut deleted = 0usize;
        for session_hash in &session_hashes {
            let removed: i64 = manager
                .del(self.key(format!("scheduler:session:{}", session_hash)))
                .await?;
            if removed > 0 {
                deleted += 1;
            }
        }
        let _: () = manager.del(set_key).await?;
        Ok(deleted)
    }

    pub async fn record_session_soft_failure(
        &self,
        session_id: &str,
        credential_id: u64,
        threshold: u32,
        ttl_secs: usize,
    ) -> anyhow::Result<bool> {
        let session_hash = session_hash(session_id);
        let script = r#"
            local raw = redis.call('GET', KEYS[1])
            if not raw then
                return 0
            end
            local ok, parsed = pcall(cjson.decode, raw)
            if not ok or not parsed['credential_id'] then
                return 0
            end
            if tostring(parsed['credential_id']) ~= ARGV[2] then
                return 0
            end
            local count = tonumber(parsed['soft_failure_count'] or '0') + 1
            parsed['soft_failure_count'] = count
            parsed['last_used_at'] = ARGV[4]
            redis.call('SET', KEYS[1], cjson.encode(parsed), 'EX', ARGV[5])
            redis.call('SADD', ARGV[6] .. ARGV[2], ARGV[1])
            redis.call('EXPIRE', ARGV[6] .. ARGV[2], ARGV[5])
            if count >= tonumber(ARGV[3]) then
                return 1
            end
            return 0
        "#;
        let mut manager = self.manager.clone();
        let should_fallback: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(self.key(session_binding_key(session_id)))
            .arg(&session_hash)
            .arg(credential_id.to_string())
            .arg(threshold)
            .arg(Utc::now().to_rfc3339())
            .arg(ttl_secs.max(1))
            .arg(self.key("scheduler:sessions_by_credential:"))
            .query_async(&mut manager)
            .await?;
        Ok(should_fallback == 1)
    }

    pub async fn clear_session_soft_failure(
        &self,
        session_id: &str,
        credential_id: u64,
        ttl_secs: usize,
    ) -> anyhow::Result<()> {
        let session_hash = session_hash(session_id);
        let script = r#"
            local raw = redis.call('GET', KEYS[1])
            if not raw then
                return 0
            end
            local ok, parsed = pcall(cjson.decode, raw)
            if not ok or not parsed['credential_id'] then
                return 0
            end
            if tostring(parsed['credential_id']) ~= ARGV[2] then
                return 0
            end
            parsed['soft_failure_count'] = 0
            parsed['last_used_at'] = ARGV[3]
            redis.call('SET', KEYS[1], cjson.encode(parsed), 'EX', ARGV[4])
            redis.call('SADD', ARGV[5] .. ARGV[2], ARGV[1])
            redis.call('EXPIRE', ARGV[5] .. ARGV[2], ARGV[4])
            return 1
        "#;
        let mut manager = self.manager.clone();
        let _: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(self.key(session_binding_key(session_id)))
            .arg(&session_hash)
            .arg(credential_id.to_string())
            .arg(Utc::now().to_rfc3339())
            .arg(ttl_secs.max(1))
            .arg(self.key("scheduler:sessions_by_credential:"))
            .query_async(&mut manager)
            .await?;
        Ok(())
    }

    pub async fn set_scheduler_cooldown(
        &self,
        credential_id: u64,
        duration: StdDuration,
        reason: Option<String>,
    ) -> anyhow::Result<SchedulerCooldownState> {
        let duration = duration.max(StdDuration::from_secs(1));
        let state = SchedulerCooldownState {
            until_ms: now_ms() + duration.as_millis() as i64,
            reason,
        };
        self.set_json(
            scheduler_cooldown_key(credential_id),
            &state,
            duration.as_secs().max(1) as usize,
        )
        .await?;
        Ok(state)
    }

    #[allow(dead_code)]
    pub async fn get_scheduler_cooldown(
        &self,
        credential_id: u64,
    ) -> anyhow::Result<Option<SchedulerCooldownState>> {
        let key = scheduler_cooldown_key(credential_id);
        let state = self.get_json::<SchedulerCooldownState>(&key).await?;
        if state
            .as_ref()
            .is_some_and(|state| state.until_ms <= now_ms())
        {
            self.del(key).await?;
            return Ok(None);
        }
        Ok(state)
    }

    pub async fn clear_scheduler_cooldown(&self, credential_id: u64) -> anyhow::Result<()> {
        self.del(scheduler_cooldown_key(credential_id)).await
    }

    pub async fn bump_rate_limit_available_at(
        &self,
        credential_id: u64,
        interval: StdDuration,
    ) -> anyhow::Result<i64> {
        let interval_ms = interval.as_millis().max(1) as i64;
        let now = now_ms();
        let script = r#"
            local current = tonumber(redis.call('GET', KEYS[1]) or '0')
            local now = tonumber(ARGV[1])
            local interval = tonumber(ARGV[2])
            local next_at = math.max(current, now) + interval
            local ttl = math.max(1, math.ceil((next_at - now) / 1000))
            redis.call('SET', KEYS[1], tostring(next_at), 'EX', ttl)
            return next_at
        "#;
        let mut manager = self.manager.clone();
        let next_at: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(self.key(scheduler_rate_limit_key(credential_id)))
            .arg(now)
            .arg(interval_ms)
            .query_async(&mut manager)
            .await?;
        Ok(next_at)
    }

    #[allow(dead_code)]
    pub async fn get_rate_limit_available_at(
        &self,
        credential_id: u64,
    ) -> anyhow::Result<Option<i64>> {
        let key = scheduler_rate_limit_key(credential_id);
        let mut manager = self.manager.clone();
        let value: Option<String> = manager.get(self.key(&key)).await?;
        let Some(value) = value else {
            return Ok(None);
        };
        let until_ms = value.parse::<i64>()?;
        if until_ms <= now_ms() {
            let _: () = manager.del(self.key(&key)).await?;
            return Ok(None);
        }
        Ok(Some(until_ms))
    }

    pub async fn clear_rate_limit(&self, credential_id: u64) -> anyhow::Result<()> {
        self.del(scheduler_rate_limit_key(credential_id)).await
    }

    pub async fn next_in_flight_lease_id(&self) -> anyhow::Result<u64> {
        let mut manager = self.manager.clone();
        let id: u64 = manager
            .incr(self.key("scheduler:inflight:lease_sequence"), 1u64)
            .await?;
        Ok(id)
    }

    pub async fn acquire_in_flight_lease(
        &self,
        credential_id: u64,
        lease_id: u64,
        max_concurrent_requests: u32,
        max_age: Option<StdDuration>,
        kind: &str,
    ) -> anyhow::Result<Option<usize>> {
        let now = now_ms();
        let max_age_ms = max_age.map(|age| age.as_millis() as i64).unwrap_or(0);
        let ttl_secs = max_age
            .map(|age| age.as_secs().saturating_mul(2).max(60) as i64)
            .unwrap_or(0);
        let script = r#"
            local now = tonumber(ARGV[1])
            local max_age_ms = tonumber(ARGV[2])
            local max_count = tonumber(ARGV[3])
            local lease_id = ARGV[4]
            local kind = ARGV[5]
            local ttl_secs = tonumber(ARGV[6])

            if max_age_ms > 0 then
                local expired = redis.call('ZRANGEBYSCORE', KEYS[1], '-inf', now - max_age_ms)
                for _, member in ipairs(expired) do
                    redis.call('ZREM', KEYS[1], member)
                    redis.call('ZREM', KEYS[2], member)
                    redis.call('HDEL', KEYS[3], member)
                end
            end

            local count = redis.call('ZCARD', KEYS[1])
            if max_count > 0 and count >= max_count then
                return {0, count}
            end

            redis.call('ZADD', KEYS[1], now, lease_id)
            redis.call('ZADD', KEYS[2], now, lease_id)
            redis.call('HSET', KEYS[3], lease_id, kind)
            if ttl_secs > 0 then
                redis.call('EXPIRE', KEYS[1], ttl_secs)
                redis.call('EXPIRE', KEYS[2], ttl_secs)
                redis.call('EXPIRE', KEYS[3], ttl_secs)
            end
            return {1, count + 1}
        "#;
        let keys = in_flight_keys(credential_id);
        let mut manager = self.manager.clone();
        let result: Vec<i64> = redis::cmd("EVAL")
            .arg(script)
            .arg(3)
            .arg(self.key(&keys.last_seen))
            .arg(self.key(&keys.acquired))
            .arg(self.key(&keys.kind))
            .arg(now)
            .arg(max_age_ms)
            .arg(max_concurrent_requests)
            .arg(lease_id.to_string())
            .arg(kind)
            .arg(ttl_secs)
            .query_async(&mut manager)
            .await?;
        if result.first().copied().unwrap_or(0) == 1 {
            Ok(Some(result.get(1).copied().unwrap_or(1).max(0) as usize))
        } else {
            Ok(None)
        }
    }

    pub async fn release_in_flight_lease(
        &self,
        credential_id: u64,
        lease_id: u64,
    ) -> anyhow::Result<bool> {
        let keys = in_flight_keys(credential_id);
        let lease_id = lease_id.to_string();
        let mut manager = self.manager.clone();
        let removed: i64 = redis::pipe()
            .atomic()
            .cmd("ZREM")
            .arg(self.key(&keys.last_seen))
            .arg(&lease_id)
            .cmd("ZREM")
            .arg(self.key(&keys.acquired))
            .arg(&lease_id)
            .cmd("HDEL")
            .arg(self.key(&keys.kind))
            .arg(&lease_id)
            .query_async::<(i64, i64, i64)>(&mut manager)
            .await
            .map(|(a, b, c)| a + b + c)?;
        Ok(removed > 0)
    }

    pub async fn touch_in_flight_lease(
        &self,
        credential_id: u64,
        lease_id: u64,
    ) -> anyhow::Result<()> {
        let keys = in_flight_keys(credential_id);
        let mut manager = self.manager.clone();
        let _: () = manager
            .zadd(
                self.key(&keys.last_seen),
                lease_id.to_string(),
                now_ms() as f64,
            )
            .await?;
        Ok(())
    }

    pub async fn set_in_flight_lease_kind(
        &self,
        credential_id: u64,
        lease_id: u64,
        kind: &str,
    ) -> anyhow::Result<()> {
        let keys = in_flight_keys(credential_id);
        let lease_id = lease_id.to_string();
        let mut manager = self.manager.clone();
        let _: (i64, i64) = redis::pipe()
            .atomic()
            .cmd("HSET")
            .arg(self.key(&keys.kind))
            .arg(&lease_id)
            .arg(kind)
            .cmd("ZADD")
            .arg(self.key(&keys.last_seen))
            .arg(now_ms() as f64)
            .arg(&lease_id)
            .query_async::<(i64, i64)>(&mut manager)
            .await?;
        Ok(())
    }

    pub async fn cleanup_expired_in_flight_leases(
        &self,
        credential_ids: &[u64],
        max_age: StdDuration,
    ) -> anyhow::Result<usize> {
        let now = now_ms();
        let max_age_ms = max_age.as_millis() as i64;
        let script = r#"
            local now = tonumber(ARGV[1])
            local max_age_ms = tonumber(ARGV[2])
            local expired = redis.call('ZRANGEBYSCORE', KEYS[1], '-inf', now - max_age_ms)
            for _, member in ipairs(expired) do
                redis.call('ZREM', KEYS[1], member)
                redis.call('ZREM', KEYS[2], member)
                redis.call('HDEL', KEYS[3], member)
            end
            return #expired
        "#;
        let mut manager = self.manager.clone();
        let mut cleaned = 0usize;
        for credential_id in credential_ids {
            let keys = in_flight_keys(*credential_id);
            let removed: i64 = redis::cmd("EVAL")
                .arg(script)
                .arg(3)
                .arg(self.key(&keys.last_seen))
                .arg(self.key(&keys.acquired))
                .arg(self.key(&keys.kind))
                .arg(now)
                .arg(max_age_ms)
                .query_async(&mut manager)
                .await?;
            cleaned += removed.max(0) as usize;
        }
        Ok(cleaned)
    }

    pub async fn clear_in_flight_leases(
        &self,
        credential_id: u64,
        min_idle: Option<StdDuration>,
    ) -> anyhow::Result<usize> {
        let keys = in_flight_keys(credential_id);
        let mut manager = self.manager.clone();
        if let Some(min_idle) = min_idle {
            let cutoff = now_ms() - min_idle.as_millis() as i64;
            let script = r#"
                local expired = redis.call('ZRANGEBYSCORE', KEYS[1], '-inf', ARGV[1])
                for _, member in ipairs(expired) do
                    redis.call('ZREM', KEYS[1], member)
                    redis.call('ZREM', KEYS[2], member)
                    redis.call('HDEL', KEYS[3], member)
                end
                return #expired
            "#;
            let removed: i64 = redis::cmd("EVAL")
                .arg(script)
                .arg(3)
                .arg(self.key(&keys.last_seen))
                .arg(self.key(&keys.acquired))
                .arg(self.key(&keys.kind))
                .arg(cutoff)
                .query_async(&mut manager)
                .await?;
            return Ok(removed.max(0) as usize);
        }

        let count: i64 = manager.zcard(self.key(&keys.last_seen)).await.unwrap_or(0);
        let _: () = redis::pipe()
            .atomic()
            .cmd("DEL")
            .arg(self.key(&keys.last_seen))
            .cmd("DEL")
            .arg(self.key(&keys.acquired))
            .cmd("DEL")
            .arg(self.key(&keys.kind))
            .query_async::<(i64, i64, i64)>(&mut manager)
            .await
            .map(|_| ())?;
        Ok(count.max(0) as usize)
    }

    pub async fn scheduler_state_for_credentials(
        &self,
        credential_ids: &[u64],
    ) -> anyhow::Result<HashMap<u64, SchedulerCredentialState>> {
        let mut states = HashMap::with_capacity(credential_ids.len());
        if credential_ids.is_empty() {
            return Ok(states);
        }

        let mut pipe = redis::pipe();
        for credential_id in credential_ids {
            let keys = in_flight_keys(*credential_id);
            pipe.cmd("GET")
                .arg(self.key(scheduler_cooldown_key(*credential_id)))
                .cmd("GET")
                .arg(self.key(scheduler_rate_limit_key(*credential_id)))
                .cmd("ZRANGE")
                .arg(self.key(&keys.last_seen))
                .arg(0)
                .arg(-1)
                .arg("WITHSCORES")
                .cmd("ZRANGE")
                .arg(self.key(&keys.acquired))
                .arg(0)
                .arg(-1)
                .arg("WITHSCORES")
                .cmd("HGETALL")
                .arg(self.key(&keys.kind));
        }

        let mut manager = self.manager.clone();
        let values: Vec<redis::Value> = pipe.query_async(&mut manager).await?;
        let now = now_ms();
        let mut keys_to_delete = Vec::new();
        for (index, credential_id) in credential_ids.iter().enumerate() {
            let base = index * 5;
            let cooldown_raw: Option<String> = redis::from_redis_value(&values[base])?;
            let rate_raw: Option<String> = redis::from_redis_value(&values[base + 1])?;
            let last_seen: Vec<(String, f64)> = redis::from_redis_value(&values[base + 2])?;
            let acquired: Vec<(String, f64)> = redis::from_redis_value(&values[base + 3])?;
            let kinds: HashMap<String, String> = redis::from_redis_value(&values[base + 4])?;

            let cooldown = cooldown_raw
                .as_deref()
                .and_then(|raw| serde_json::from_str::<SchedulerCooldownState>(raw).ok())
                .and_then(|state| {
                    if state.until_ms <= now {
                        keys_to_delete.push(scheduler_cooldown_key(*credential_id));
                        None
                    } else {
                        Some(state)
                    }
                });
            let rate_limit_available_at_ms = rate_raw
                .as_deref()
                .and_then(|raw| raw.parse::<i64>().ok())
                .and_then(|until_ms| {
                    if until_ms <= now {
                        keys_to_delete.push(scheduler_rate_limit_key(*credential_id));
                        None
                    } else {
                        Some(until_ms)
                    }
                });
            let acquired_map: HashMap<String, i64> = acquired
                .into_iter()
                .map(|(member, score)| (member, score as i64))
                .collect();
            let in_flight_leases = last_seen
                .into_iter()
                .filter_map(|(member, last_seen_score)| {
                    let id = member.parse::<u64>().ok()?;
                    let acquired_at_ms = acquired_map.get(&member).copied()?;
                    Some(SchedulerInFlightLease {
                        id,
                        acquired_at_ms,
                        last_seen_at_ms: last_seen_score as i64,
                        kind: kinds
                            .get(&member)
                            .cloned()
                            .unwrap_or_else(|| "api".to_string()),
                    })
                })
                .collect();
            states.insert(
                *credential_id,
                SchedulerCredentialState {
                    cooldown,
                    rate_limit_available_at_ms,
                    in_flight_leases,
                },
            );
        }
        if !keys_to_delete.is_empty() {
            let mut manager = self.manager.clone();
            let full_keys: Vec<String> = keys_to_delete
                .into_iter()
                .map(|key| self.key(key))
                .collect();
            let _: () = manager.del(full_keys).await?;
        }
        Ok(states)
    }

    pub async fn acquire_refresh_lock(
        &self,
        credential_id: u64,
        ttl_secs: usize,
    ) -> anyhow::Result<Option<String>> {
        let token = uuid::Uuid::new_v4().to_string();
        let acquired = self
            .set_nx_ex(scheduler_refresh_lock_key(credential_id), &token, ttl_secs)
            .await?;
        Ok(acquired.then_some(token))
    }

    pub async fn release_refresh_lock(
        &self,
        credential_id: u64,
        token: &str,
    ) -> anyhow::Result<bool> {
        self.release_lock(scheduler_refresh_lock_key(credential_id), token)
            .await
    }
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn session_hash(session_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(session_id.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn session_binding_key(session_id: &str) -> String {
    format!("scheduler:session:{}", session_hash(session_id))
}

fn sessions_by_credential_key(credential_id: u64) -> String {
    format!("scheduler:sessions_by_credential:{}", credential_id)
}

fn scheduler_cooldown_key(credential_id: u64) -> String {
    format!("scheduler:cooldown:{}", credential_id)
}

fn scheduler_rate_limit_key(credential_id: u64) -> String {
    format!("scheduler:rate_limit:{}", credential_id)
}

fn scheduler_refresh_lock_key(credential_id: u64) -> String {
    format!("scheduler:refresh_lock:{}", credential_id)
}

struct InFlightKeys {
    last_seen: String,
    acquired: String,
    kind: String,
}

fn in_flight_keys(credential_id: u64) -> InFlightKeys {
    InFlightKeys {
        last_seen: format!("scheduler:inflight:{}:last_seen", credential_id),
        acquired: format!("scheduler:inflight:{}:acquired", credential_id),
        kind: format!("scheduler:inflight:{}:kind", credential_id),
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::model::config::Config;

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    struct CachedValue {
        value: String,
    }

    fn test_config() -> Option<Config> {
        let url = std::env::var("KIRO_RS_TEST_REDIS_URL").ok()?;
        let mut config = Config::default();
        config.redis.url = Some(url);
        config.redis.key_prefix = format!("kiro_rs:test:{}", uuid::Uuid::new_v4());
        Some(config)
    }

    #[tokio::test]
    async fn redis_json_round_trip_and_delete() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let value = CachedValue {
            value: "ok".to_string(),
        };

        store.set_json("sample", &value, 60).await.unwrap();
        assert_eq!(
            store.get_json::<CachedValue>("sample").await.unwrap(),
            Some(value)
        );

        store.del("sample").await.unwrap();
        assert_eq!(store.get_json::<CachedValue>("sample").await.unwrap(), None);
    }

    #[tokio::test]
    async fn redis_scheduler_session_binding_round_trip_and_soft_failure() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let binding = SchedulerSessionBinding {
            credential_id: 7,
            last_used_at: Utc::now(),
            soft_failure_count: 0,
        };
        store
            .set_session_binding("session-a", &binding, 60)
            .await
            .unwrap();
        let loaded = store
            .get_session_binding("session-a")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.credential_id, 7);
        assert_eq!(loaded.soft_failure_count, 0);

        let rebound = SchedulerSessionBinding {
            credential_id: 8,
            last_used_at: Utc::now(),
            soft_failure_count: 0,
        };
        store
            .set_session_binding("session-a", &rebound, 60)
            .await
            .unwrap();
        assert_eq!(store.delete_sessions_for_credential(7).await.unwrap(), 0);
        let loaded = store
            .get_session_binding("session-a")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.credential_id, 8);
        assert_eq!(loaded.soft_failure_count, 0);

        assert!(
            !store
                .record_session_soft_failure("session-a", 8, 2, 60)
                .await
                .unwrap()
        );
        assert!(
            store
                .record_session_soft_failure("session-a", 8, 2, 60)
                .await
                .unwrap()
        );
        store
            .clear_session_soft_failure("session-a", 8, 60)
            .await
            .unwrap();
        let loaded = store
            .get_session_binding("session-a")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.soft_failure_count, 0);

        assert_eq!(store.delete_sessions_for_credential(8).await.unwrap(), 1);
        assert!(
            store
                .get_session_binding("session-a")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn redis_runtime_event_pubsub_round_trip() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let mut pubsub = store.subscribe_runtime_events().await.unwrap();
        let expected_channel = store.runtime_config_changed_channel();
        let mut stream = pubsub.on_message();

        store
            .publish_runtime_config_changed(r#"{"kind":"runtime_config_changed"}"#)
            .await
            .unwrap();

        let message = tokio::time::timeout(StdDuration::from_secs(2), stream.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.get_channel_name(), expected_channel);
        assert_eq!(
            message.get_payload::<String>().unwrap(),
            r#"{"kind":"runtime_config_changed"}"#
        );
    }

    #[tokio::test]
    async fn redis_scheduler_cooldown_and_rate_limit_round_trip() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let cooldown = store
            .set_scheduler_cooldown(3, StdDuration::from_secs(30), Some("429".to_string()))
            .await
            .unwrap();
        assert!(cooldown.until_ms > now_ms());
        let loaded = store.get_scheduler_cooldown(3).await.unwrap().unwrap();
        assert_eq!(loaded.reason.as_deref(), Some("429"));
        store.clear_scheduler_cooldown(3).await.unwrap();
        assert!(store.get_scheduler_cooldown(3).await.unwrap().is_none());

        let first = store
            .bump_rate_limit_available_at(3, StdDuration::from_millis(50))
            .await
            .unwrap();
        let second = store
            .bump_rate_limit_available_at(3, StdDuration::from_millis(50))
            .await
            .unwrap();
        assert!(second > first);
        assert_eq!(
            store.get_rate_limit_available_at(3).await.unwrap(),
            Some(second)
        );
        store.clear_rate_limit(3).await.unwrap();
        assert!(
            store
                .get_rate_limit_available_at(3)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn redis_scheduler_in_flight_acquire_release_and_cleanup() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let lease_a = store.next_in_flight_lease_id().await.unwrap();
        assert!(
            store
                .acquire_in_flight_lease(9, lease_a, 1, Some(StdDuration::from_secs(60)), "api",)
                .await
                .unwrap()
                .is_some()
        );
        let lease_b = store.next_in_flight_lease_id().await.unwrap();
        assert!(
            store
                .acquire_in_flight_lease(9, lease_b, 1, Some(StdDuration::from_secs(60)), "api",)
                .await
                .unwrap()
                .is_none()
        );

        let state = store.scheduler_state_for_credentials(&[9]).await.unwrap();
        assert_eq!(state.get(&9).unwrap().in_flight_leases.len(), 1);
        assert!(store.release_in_flight_lease(9, lease_a).await.unwrap());
        let state = store.scheduler_state_for_credentials(&[9]).await.unwrap();
        assert_eq!(state.get(&9).unwrap().in_flight_leases.len(), 0);

        assert!(
            store
                .acquire_in_flight_lease(
                    9,
                    lease_b,
                    1,
                    Some(StdDuration::from_millis(1)),
                    "stream",
                )
                .await
                .unwrap()
                .is_some()
        );
        tokio::time::sleep(StdDuration::from_millis(5)).await;
        assert_eq!(
            store
                .cleanup_expired_in_flight_leases(&[9], StdDuration::from_millis(1))
                .await
                .unwrap(),
            1
        );
        let state = store.scheduler_state_for_credentials(&[9]).await.unwrap();
        assert_eq!(state.get(&9).unwrap().in_flight_leases.len(), 0);
    }

    #[tokio::test]
    async fn redis_scheduler_refresh_lock_is_exclusive() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let lock = store.acquire_refresh_lock(11, 30).await.unwrap().unwrap();
        assert!(store.acquire_refresh_lock(11, 30).await.unwrap().is_none());
        assert!(store.release_refresh_lock(11, &lock).await.unwrap());
        assert!(store.acquire_refresh_lock(11, 30).await.unwrap().is_some());
    }
}
