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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SchedulerHealthState {
    pub transient_failure_streak: u32,
    pub recent_error_rate: f64,
    pub latency_ewma_ms: Option<f64>,
    pub last_error_kind: Option<String>,
    pub last_error_reason: Option<String>,
    pub last_error_at_ms: Option<i64>,
    pub probation_until_ms: Option<i64>,
    pub selection_count: u64,
    pub recent_selection_count_10s: u32,
    pub recent_selection_count_60s: u32,
    pub recent_selection_count_5m: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerInFlightLease {
    pub id: u64,
    pub acquired_at_ms: i64,
    pub last_seen_at_ms: i64,
    pub kind: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SchedulerCredentialState {
    pub cooldown: Option<SchedulerCooldownState>,
    pub health: SchedulerHealthState,
    pub model_states: Vec<SchedulerModelState>,
    pub rate_limit_available_at_ms: Option<i64>,
    pub in_flight_leases: Vec<SchedulerInFlightLease>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SchedulerModelState {
    pub model: String,
    pub cooldown: Option<SchedulerCooldownState>,
    pub health: SchedulerHealthState,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SchedulerGlobalCapacityState {
    pub in_flight_requests: u32,
    pub queued_requests: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExternalPoolCapacityState {
    pub pool_in_flight_requests: u32,
    pub global_in_flight_requests: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalPoolCircuitState {
    pub open: bool,
    pub open_until_ms: Option<i64>,
    pub reason: Option<String>,
    pub recent_failures: u32,
    pub distinct_credentials: u32,
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

    pub async fn incr_with_ttl(
        &self,
        key: impl AsRef<str>,
        ttl_secs: usize,
    ) -> anyhow::Result<u64> {
        let script = r#"
            local value = redis.call('INCR', KEYS[1])
            if value == 1 then
                redis.call('EXPIRE', KEYS[1], ARGV[1])
            end
            return value
        "#;
        let mut manager = self.manager.clone();
        let value: u64 = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(self.key(key))
            .arg(ttl_secs.max(1))
            .query_async(&mut manager)
            .await?;
        Ok(value)
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

    #[allow(dead_code)]
    pub async fn set_scheduler_cooldown(
        &self,
        credential_id: u64,
        duration: StdDuration,
        reason: Option<String>,
    ) -> anyhow::Result<SchedulerCooldownState> {
        let duration = duration.max(StdDuration::from_secs(1));
        let now = now_ms();
        let state = SchedulerCooldownState {
            until_ms: now + duration.as_millis() as i64,
            reason,
            model: None,
        };
        let encoded = serde_json::to_string(&state)?;
        let ttl_ms = (state.until_ms - now).max(1);
        let script = r#"
            local existing = redis.call('GET', KEYS[1])
            if existing then
                local ok, existing_data = pcall(cjson.decode, existing)
                if ok and existing_data and existing_data.until_ms then
                    local existing_until = tonumber(existing_data.until_ms)
                    local new_until = tonumber(ARGV[1])
                    if existing_until and new_until and existing_until >= new_until then
                        return existing
                    end
                end
            end
            redis.call('SET', KEYS[1], ARGV[2], 'PX', ARGV[3])
            return ARGV[2]
        "#;
        let mut manager = self.manager.clone();
        let stored: String = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(self.key(scheduler_cooldown_key(credential_id)))
            .arg(state.until_ms)
            .arg(&encoded)
            .arg(ttl_ms)
            .query_async(&mut manager)
            .await?;
        Ok(serde_json::from_str(&stored)?)
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
        let mut manager = self.manager.clone();
        let index_key = scheduler_model_index_key(credential_id);
        let models: HashMap<String, String> = manager
            .hgetall(self.key(&index_key))
            .await
            .unwrap_or_default();
        let mut pipe = redis::pipe();
        pipe.cmd("DEL")
            .arg(self.key(scheduler_cooldown_key(credential_id)));
        for hash in models.keys() {
            pipe.cmd("DEL")
                .arg(self.key(scheduler_model_cooldown_key(credential_id, hash)));
        }
        let _: () = pipe.query_async(&mut manager).await?;
        Ok(())
    }

    pub async fn record_scheduler_transient_failure(
        &self,
        credential_id: u64,
        model: Option<&str>,
        kind: &str,
        reason: &str,
        retry_after: Option<StdDuration>,
        base_cooldown: StdDuration,
        max_cooldown: StdDuration,
        backoff_multiplier: f64,
        jitter_factor: f64,
        probation: StdDuration,
        ewma_alpha: f64,
    ) -> anyhow::Result<(SchedulerCooldownState, SchedulerHealthState)> {
        let now = now_ms();
        let retry_after_ms = retry_after
            .map(|duration| duration.as_millis().max(1) as i64)
            .unwrap_or(-1);
        let model = model.map(str::trim).filter(|value| !value.is_empty());
        let model_hash = model.map(scheduler_model_hash);
        let cooldown_key = match model_hash.as_deref() {
            Some(hash) => scheduler_model_cooldown_key(credential_id, hash),
            None => scheduler_cooldown_key(credential_id),
        };
        let health_key = match model_hash.as_deref() {
            Some(hash) => scheduler_model_health_key(credential_id, hash),
            None => scheduler_health_key(credential_id),
        };
        let script = r#"
            local now = tonumber(ARGV[1])
            local kind = ARGV[2]
            local reason = ARGV[3]
            local retry_after_ms = tonumber(ARGV[4])
            local base_ms = tonumber(ARGV[5])
            local max_ms = tonumber(ARGV[6])
            local multiplier = tonumber(ARGV[7])
            local jitter = tonumber(ARGV[8])
            local probation_ms = tonumber(ARGV[9])
            local alpha = tonumber(ARGV[10])
            local health_ttl = tonumber(ARGV[11])
            local model = ARGV[12]
            local model_hash = ARGV[13]

            local health = {}
            local health_raw = redis.call('GET', KEYS[2])
            if health_raw then
                local ok, parsed = pcall(cjson.decode, health_raw)
                if ok and parsed then health = parsed end
            end
            local streak = tonumber(health['transient_failure_streak'] or '0') + 1
            local previous_error_rate = tonumber(health['recent_error_rate'] or '0')
            health['transient_failure_streak'] = streak
            health['recent_error_rate'] = previous_error_rate + alpha * (1 - previous_error_rate)
            health['last_error_kind'] = kind
            health['last_error_reason'] = reason
            health['last_error_at_ms'] = now

            local requested
            if retry_after_ms >= 0 then
                requested = retry_after_ms
            else
                requested = base_ms * (multiplier ^ math.max(streak - 1, 0)) * jitter
            end
            local duration_ms = math.max(1000, math.min(max_ms, math.floor(requested + 0.5)))
            local candidate_until = now + duration_ms

            local cooldown = {until_ms = candidate_until, reason = reason}
            if model ~= '' then cooldown['model'] = model end
            local cooldown_raw = redis.call('GET', KEYS[1])
            if cooldown_raw then
                local ok, parsed = pcall(cjson.decode, cooldown_raw)
                if ok and parsed and tonumber(parsed['until_ms'] or '0') >= candidate_until then
                    cooldown = parsed
                end
            end

            local current_probation = tonumber(health['probation_until_ms'] or '0')
            health['probation_until_ms'] = math.max(current_probation, tonumber(cooldown['until_ms']) + probation_ms)
            local health_encoded = cjson.encode(health)
            local cooldown_encoded = cjson.encode(cooldown)
            redis.call('SET', KEYS[2], health_encoded, 'EX', health_ttl)
            redis.call('SET', KEYS[1], cooldown_encoded, 'PX', math.max(1, tonumber(cooldown['until_ms']) - now))
            if model ~= '' then
                redis.call('HSET', KEYS[3], model_hash, model)
                redis.call('EXPIRE', KEYS[3], health_ttl)
            end
            return {cooldown_encoded, health_encoded}
        "#;
        let health_ttl_secs = 30 * 24 * 60 * 60;
        let mut manager = self.manager.clone();
        let result: Vec<String> = redis::cmd("EVAL")
            .arg(script)
            .arg(3)
            .arg(self.key(cooldown_key))
            .arg(self.key(health_key))
            .arg(self.key(scheduler_model_index_key(credential_id)))
            .arg(now)
            .arg(kind)
            .arg(reason)
            .arg(retry_after_ms)
            .arg(base_cooldown.as_millis().max(1) as i64)
            .arg(max_cooldown.as_millis().max(1) as i64)
            .arg(backoff_multiplier.max(1.0))
            .arg(jitter_factor.max(0.01))
            .arg(probation.as_millis() as i64)
            .arg(ewma_alpha.clamp(0.01, 1.0))
            .arg(health_ttl_secs)
            .arg(model.unwrap_or(""))
            .arg(model_hash.as_deref().unwrap_or(""))
            .query_async(&mut manager)
            .await?;
        let cooldown = serde_json::from_str(
            result
                .first()
                .ok_or_else(|| anyhow::anyhow!("Redis 未返回调度冷却结果"))?,
        )?;
        let health = serde_json::from_str(
            result
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("Redis 未返回调度健康结果"))?,
        )?;
        Ok((cooldown, health))
    }

    pub async fn record_scheduler_success(
        &self,
        credential_id: u64,
        model: Option<&str>,
        latency: Option<StdDuration>,
        ewma_alpha: f64,
    ) -> anyhow::Result<SchedulerHealthState> {
        let model = model.map(str::trim).filter(|value| !value.is_empty());
        let model_hash = model.map(scheduler_model_hash);
        let health_key = match model_hash.as_deref() {
            Some(hash) => scheduler_model_health_key(credential_id, hash),
            None => scheduler_health_key(credential_id),
        };
        let script = r#"
            local alpha = tonumber(ARGV[1])
            local latency_ms = tonumber(ARGV[2])
            local ttl = tonumber(ARGV[3])
            local model = ARGV[4]
            local model_hash = ARGV[5]
            local health = {}
            local raw = redis.call('GET', KEYS[1])
            if raw then
                local ok, parsed = pcall(cjson.decode, raw)
                if ok and parsed then health = parsed end
            end
            local previous_error_rate = tonumber(health['recent_error_rate'] or '0')
            health['recent_error_rate'] = previous_error_rate * (1 - alpha)
            health['transient_failure_streak'] = math.max(0, tonumber(health['transient_failure_streak'] or '0') - 1)
            if latency_ms >= 0 then
                local previous_latency = tonumber(health['latency_ewma_ms'])
                if previous_latency then
                    health['latency_ewma_ms'] = previous_latency + alpha * (latency_ms - previous_latency)
                else
                    health['latency_ewma_ms'] = latency_ms
                end
            end
            local encoded = cjson.encode(health)
            redis.call('SET', KEYS[1], encoded, 'EX', ttl)
            if model ~= '' then
                redis.call('HSET', KEYS[2], model_hash, model)
                redis.call('EXPIRE', KEYS[2], ttl)
            end
            return encoded
        "#;
        let latency_ms = latency
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or(-1);
        let mut manager = self.manager.clone();
        let encoded: String = redis::cmd("EVAL")
            .arg(script)
            .arg(2)
            .arg(self.key(health_key))
            .arg(self.key(scheduler_model_index_key(credential_id)))
            .arg(ewma_alpha.clamp(0.01, 1.0))
            .arg(latency_ms)
            .arg(30 * 24 * 60 * 60)
            .arg(model.unwrap_or(""))
            .arg(model_hash.as_deref().unwrap_or(""))
            .query_async(&mut manager)
            .await?;
        Ok(serde_json::from_str(&encoded)?)
    }

    pub async fn record_scheduler_selection(
        &self,
        credential_id: u64,
    ) -> anyhow::Result<SchedulerHealthState> {
        let now = now_ms();
        let script = r#"
            local ttl = tonumber(ARGV[1])
            local now = tonumber(ARGV[2])
            local window_10s = tonumber(ARGV[3])
            local window_60s = tonumber(ARGV[4])
            local window_5m = tonumber(ARGV[5])
            local health = {}
            local raw = redis.call('GET', KEYS[1])
            if raw then
                local ok, parsed = pcall(cjson.decode, raw)
                if ok and parsed then health = parsed end
            end
            local sequence = redis.call('INCR', KEYS[3])
            local member = tostring(now) .. '-' .. tostring(sequence)
            redis.call('ZREMRANGEBYSCORE', KEYS[2], '-inf', now - window_5m)
            redis.call('ZADD', KEYS[2], now, member)
            redis.call('EXPIRE', KEYS[2], ttl)
            health['selection_count'] = tonumber(health['selection_count'] or '0') + 1
            health['recent_selection_count_10s'] = redis.call('ZCOUNT', KEYS[2], now - window_10s, '+inf')
            health['recent_selection_count_60s'] = redis.call('ZCOUNT', KEYS[2], now - window_60s, '+inf')
            health['recent_selection_count_5m'] = redis.call('ZCOUNT', KEYS[2], now - window_5m, '+inf')
            redis.call('SET', KEYS[1], cjson.encode(health), 'EX', ttl)
            return cjson.encode(health)
        "#;
        let mut manager = self.manager.clone();
        let encoded: String = redis::cmd("EVAL")
            .arg(script)
            .arg(3)
            .arg(self.key(scheduler_health_key(credential_id)))
            .arg(self.key(scheduler_selection_window_key(credential_id)))
            .arg(self.key("scheduler:selection:sequence"))
            .arg(30 * 24 * 60 * 60)
            .arg(now)
            .arg(10_000)
            .arg(60_000)
            .arg(5 * 60_000)
            .query_async(&mut manager)
            .await?;
        Ok(serde_json::from_str(&encoded)?)
    }

    pub async fn clear_scheduler_health(&self, credential_id: u64) -> anyhow::Result<()> {
        let mut manager = self.manager.clone();
        let index_key = scheduler_model_index_key(credential_id);
        let models: HashMap<String, String> = manager
            .hgetall(self.key(&index_key))
            .await
            .unwrap_or_default();
        let mut pipe = redis::pipe();
        pipe.atomic()
            .cmd("DEL")
            .arg(self.key(scheduler_health_key(credential_id)))
            .cmd("DEL")
            .arg(self.key(scheduler_selection_window_key(credential_id)))
            .cmd("DEL")
            .arg(self.key(&index_key));
        for hash in models.keys() {
            pipe.cmd("DEL")
                .arg(self.key(scheduler_model_health_key(credential_id, hash)))
                .cmd("DEL")
                .arg(self.key(scheduler_model_cooldown_key(credential_id, hash)));
        }
        let _: () = pipe.query_async(&mut manager).await?;
        Ok(())
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

    pub async fn next_external_pool_lease_id(&self) -> anyhow::Result<u64> {
        let mut manager = self.manager.clone();
        let id: u64 = manager
            .incr(self.key("external_pool:inflight:lease_sequence"), 1u64)
            .await?;
        Ok(id)
    }

    #[allow(dead_code)]
    pub async fn acquire_in_flight_lease(
        &self,
        credential_id: u64,
        lease_id: u64,
        max_concurrent_requests: u32,
        max_age: Option<StdDuration>,
        kind: &str,
    ) -> anyhow::Result<Option<usize>> {
        self.acquire_dispatch_lease(
            credential_id,
            lease_id,
            max_concurrent_requests,
            0,
            max_age,
            kind,
        )
        .await
    }

    pub async fn acquire_dispatch_lease(
        &self,
        credential_id: u64,
        lease_id: u64,
        max_concurrent_requests: u32,
        global_max_concurrent_requests: u32,
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
            local global_max_count = tonumber(ARGV[4])
            local lease_id = ARGV[5]
            local kind = ARGV[6]
            local ttl_secs = tonumber(ARGV[7])

            if max_age_ms > 0 then
                local expired = redis.call('ZRANGEBYSCORE', KEYS[1], '-inf', now - max_age_ms)
                for _, member in ipairs(expired) do
                    redis.call('ZREM', KEYS[1], member)
                    redis.call('ZREM', KEYS[2], member)
                    redis.call('HDEL', KEYS[3], member)
                end
                local global_expired = redis.call('ZRANGEBYSCORE', KEYS[4], '-inf', now - max_age_ms)
                for _, member in ipairs(global_expired) do
                    redis.call('ZREM', KEYS[4], member)
                    redis.call('ZREM', KEYS[5], member)
                    redis.call('HDEL', KEYS[6], member)
                end
            end

            local count = redis.call('ZCARD', KEYS[1])
            if max_count > 0 and count >= max_count then
                return {0, count}
            end

            local global_count = redis.call('ZCARD', KEYS[4])
            if global_max_count > 0 and global_count >= global_max_count then
                return {0, global_count}
            end

            redis.call('ZADD', KEYS[1], now, lease_id)
            redis.call('ZADD', KEYS[2], now, lease_id)
            redis.call('HSET', KEYS[3], lease_id, kind)
            redis.call('ZADD', KEYS[4], now, lease_id)
            redis.call('ZADD', KEYS[5], now, lease_id)
            redis.call('HSET', KEYS[6], lease_id, kind)
            if ttl_secs > 0 then
                redis.call('EXPIRE', KEYS[1], ttl_secs)
                redis.call('EXPIRE', KEYS[2], ttl_secs)
                redis.call('EXPIRE', KEYS[3], ttl_secs)
                redis.call('EXPIRE', KEYS[4], ttl_secs)
                redis.call('EXPIRE', KEYS[5], ttl_secs)
                redis.call('EXPIRE', KEYS[6], ttl_secs)
            end
            return {1, count + 1}
        "#;
        let keys = in_flight_keys(credential_id);
        let global_keys = global_in_flight_keys();
        let mut manager = self.manager.clone();
        let result: Vec<i64> = redis::cmd("EVAL")
            .arg(script)
            .arg(6)
            .arg(self.key(&keys.last_seen))
            .arg(self.key(&keys.acquired))
            .arg(self.key(&keys.kind))
            .arg(self.key(&global_keys.last_seen))
            .arg(self.key(&global_keys.acquired))
            .arg(self.key(&global_keys.kind))
            .arg(now)
            .arg(max_age_ms)
            .arg(max_concurrent_requests)
            .arg(global_max_concurrent_requests)
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

    pub async fn acquire_external_pool_lease(
        &self,
        pool_id: u64,
        lease_id: u64,
        max_concurrent_requests: u32,
        global_max_concurrent_requests: u32,
        max_age: Option<StdDuration>,
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
            local global_max_count = tonumber(ARGV[4])
            local lease_id = ARGV[5]
            local ttl_secs = tonumber(ARGV[6])

            if max_age_ms > 0 then
                local expired = redis.call('ZRANGEBYSCORE', KEYS[1], '-inf', now - max_age_ms)
                for _, member in ipairs(expired) do
                    redis.call('ZREM', KEYS[1], member)
                    redis.call('ZREM', KEYS[2], member)
                end
                local global_expired = redis.call('ZRANGEBYSCORE', KEYS[3], '-inf', now - max_age_ms)
                for _, member in ipairs(global_expired) do
                    redis.call('ZREM', KEYS[3], member)
                    redis.call('ZREM', KEYS[4], member)
                end
            end

            local count = redis.call('ZCARD', KEYS[1])
            if max_count > 0 and count >= max_count then
                return {0, count}
            end

            local global_count = redis.call('ZCARD', KEYS[3])
            if global_max_count > 0 and global_count >= global_max_count then
                return {0, global_count}
            end

            redis.call('ZADD', KEYS[1], now, lease_id)
            redis.call('ZADD', KEYS[2], now, lease_id)
            redis.call('ZADD', KEYS[3], now, lease_id)
            redis.call('ZADD', KEYS[4], now, lease_id)
            if ttl_secs > 0 then
                redis.call('EXPIRE', KEYS[1], ttl_secs)
                redis.call('EXPIRE', KEYS[2], ttl_secs)
                redis.call('EXPIRE', KEYS[3], ttl_secs)
                redis.call('EXPIRE', KEYS[4], ttl_secs)
            end
            return {1, count + 1}
        "#;
        let keys = external_pool_in_flight_keys(pool_id);
        let global_keys = external_pool_global_in_flight_keys();
        let mut manager = self.manager.clone();
        let result: Vec<i64> = redis::cmd("EVAL")
            .arg(script)
            .arg(4)
            .arg(self.key(&keys.last_seen))
            .arg(self.key(&keys.acquired))
            .arg(self.key(&global_keys.last_seen))
            .arg(self.key(&global_keys.acquired))
            .arg(now)
            .arg(max_age_ms)
            .arg(max_concurrent_requests)
            .arg(global_max_concurrent_requests)
            .arg(lease_id.to_string())
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
        let global_keys = global_in_flight_keys();
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
            .cmd("ZREM")
            .arg(self.key(&global_keys.last_seen))
            .arg(&lease_id)
            .cmd("ZREM")
            .arg(self.key(&global_keys.acquired))
            .arg(&lease_id)
            .cmd("HDEL")
            .arg(self.key(&global_keys.kind))
            .arg(&lease_id)
            .query_async::<(i64, i64, i64, i64, i64, i64)>(&mut manager)
            .await
            .map(|(a, b, c, d, e, f)| a + b + c + d + e + f)?;
        Ok(removed > 0)
    }

    pub async fn release_external_pool_lease(
        &self,
        pool_id: u64,
        lease_id: u64,
    ) -> anyhow::Result<bool> {
        let keys = external_pool_in_flight_keys(pool_id);
        let global_keys = external_pool_global_in_flight_keys();
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
            .cmd("ZREM")
            .arg(self.key(&global_keys.last_seen))
            .arg(&lease_id)
            .cmd("ZREM")
            .arg(self.key(&global_keys.acquired))
            .arg(&lease_id)
            .query_async::<(i64, i64, i64, i64)>(&mut manager)
            .await
            .map(|(a, b, c, d)| a + b + c + d)?;
        Ok(removed > 0)
    }

    pub async fn touch_external_pool_lease(
        &self,
        pool_id: u64,
        lease_id: u64,
        ttl_secs: usize,
    ) -> anyhow::Result<bool> {
        let keys = external_pool_in_flight_keys(pool_id);
        let global_keys = external_pool_global_in_flight_keys();
        let lease_id = lease_id.to_string();
        let now = now_ms();
        let script = r#"
            local lease_id = ARGV[1]
            local now = tonumber(ARGV[2])
            local ttl_secs = tonumber(ARGV[3])

            if not redis.call('ZSCORE', KEYS[2], lease_id) then
                return 0
            end
            if not redis.call('ZSCORE', KEYS[4], lease_id) then
                return 0
            end

            redis.call('ZADD', KEYS[1], now, lease_id)
            redis.call('ZADD', KEYS[3], now, lease_id)
            if ttl_secs > 0 then
                redis.call('EXPIRE', KEYS[1], ttl_secs)
                redis.call('EXPIRE', KEYS[2], ttl_secs)
                redis.call('EXPIRE', KEYS[3], ttl_secs)
                redis.call('EXPIRE', KEYS[4], ttl_secs)
            end
            return 1
        "#;
        let mut manager = self.manager.clone();
        let touched: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(4)
            .arg(self.key(&keys.last_seen))
            .arg(self.key(&keys.acquired))
            .arg(self.key(&global_keys.last_seen))
            .arg(self.key(&global_keys.acquired))
            .arg(lease_id)
            .arg(now)
            .arg(ttl_secs.max(1))
            .query_async(&mut manager)
            .await?;
        Ok(touched == 1)
    }

    pub async fn external_pool_capacity_state(
        &self,
        pool_id: u64,
        max_age: Option<StdDuration>,
    ) -> anyhow::Result<ExternalPoolCapacityState> {
        let now = now_ms();
        let max_age_ms = max_age.map(|age| age.as_millis() as i64).unwrap_or(0);
        let script = r#"
            local now = tonumber(ARGV[1])
            local max_age_ms = tonumber(ARGV[2])
            if max_age_ms > 0 then
                local expired = redis.call('ZRANGEBYSCORE', KEYS[1], '-inf', now - max_age_ms)
                for _, member in ipairs(expired) do
                    redis.call('ZREM', KEYS[1], member)
                    redis.call('ZREM', KEYS[2], member)
                end
                local global_expired = redis.call('ZRANGEBYSCORE', KEYS[3], '-inf', now - max_age_ms)
                for _, member in ipairs(global_expired) do
                    redis.call('ZREM', KEYS[3], member)
                    redis.call('ZREM', KEYS[4], member)
                end
            end
            return {redis.call('ZCARD', KEYS[1]), redis.call('ZCARD', KEYS[3])}
        "#;
        let keys = external_pool_in_flight_keys(pool_id);
        let global_keys = external_pool_global_in_flight_keys();
        let mut manager = self.manager.clone();
        let result: Vec<i64> = redis::cmd("EVAL")
            .arg(script)
            .arg(4)
            .arg(self.key(&keys.last_seen))
            .arg(self.key(&keys.acquired))
            .arg(self.key(&global_keys.last_seen))
            .arg(self.key(&global_keys.acquired))
            .arg(now)
            .arg(max_age_ms)
            .query_async(&mut manager)
            .await?;
        Ok(ExternalPoolCapacityState {
            pool_in_flight_requests: result.first().copied().unwrap_or(0).max(0) as u32,
            global_in_flight_requests: result.get(1).copied().unwrap_or(0).max(0) as u32,
        })
    }

    pub async fn record_local_pool_circuit_failure(
        &self,
        credential_id: Option<u64>,
        reason: &str,
        window: StdDuration,
        open_after_failures: u32,
        require_distinct_credentials: u32,
        open_for: StdDuration,
    ) -> anyhow::Result<LocalPoolCircuitState> {
        let now = now_ms();
        let window_ms = window.as_millis().max(1) as i64;
        let open_for_ms = open_for.as_millis().max(1) as i64;
        let credential_member = credential_id
            .map(|id| format!("credential:{}", id))
            .unwrap_or_else(|| "unknown".to_string());
        let script = r#"
            local now = tonumber(ARGV[1])
            local window_ms = tonumber(ARGV[2])
            local credential = ARGV[3]
            local reason = ARGV[4]
            local open_after = tonumber(ARGV[5])
            local required_distinct = tonumber(ARGV[6])
            local open_for_ms = tonumber(ARGV[7])
            local ttl_secs = tonumber(ARGV[8])

            redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now - window_ms)
            local seq = redis.call('INCR', KEYS[2])
            redis.call('PEXPIRE', KEYS[2], (ttl_secs * 1000))
            redis.call('ZADD', KEYS[1], now, 'failure:' .. tostring(seq) .. ':' .. credential)
            redis.call('ZADD', KEYS[1], now, credential)

            local failure_members = redis.call('ZRANGEBYSCORE', KEYS[1], now - window_ms + 1, now)
            local failures = 0
            local distinct_credentials = {}
            for _, member in ipairs(failure_members) do
                if string.sub(member, 1, 8) == 'failure:' then
                    failures = failures + 1
                else
                    distinct_credentials[member] = true
                end
            end
            local distinct = 0
            for _, _ in pairs(distinct_credentials) do
                distinct = distinct + 1
            end
            local open_until = tonumber(redis.call('GET', KEYS[3]) or '0')
            local reported_reason = ''
            local opened = 0
            if open_until > now then
                opened = 1
                reported_reason = redis.call('GET', KEYS[4]) or reason
            elseif failures >= open_after and distinct >= required_distinct then
                open_until = now + open_for_ms
                redis.call('SET', KEYS[3], open_until, 'PX', open_for_ms)
                redis.call('SET', KEYS[4], reason, 'PX', open_for_ms)
                opened = 1
                reported_reason = reason
            else
                open_until = 0
            end

            redis.call('EXPIRE', KEYS[1], ttl_secs)
            return {opened, open_until, reported_reason, failures, distinct}
        "#;
        let ttl_secs = window.as_secs().saturating_add(open_for.as_secs()).max(1) as usize;
        let mut manager = self.manager.clone();
        let result: Vec<redis::Value> = redis::cmd("EVAL")
            .arg(script)
            .arg(4)
            .arg(self.key(local_pool_circuit_failures_key()))
            .arg(self.key(local_pool_circuit_sequence_key()))
            .arg(self.key(local_pool_circuit_open_until_key()))
            .arg(self.key(local_pool_circuit_reason_key()))
            .arg(now)
            .arg(window_ms)
            .arg(credential_member)
            .arg(reason)
            .arg(open_after_failures.max(1))
            .arg(require_distinct_credentials.max(1))
            .arg(open_for_ms)
            .arg(ttl_secs)
            .query_async(&mut manager)
            .await?;
        Ok(LocalPoolCircuitState {
            open: redis::from_redis_value::<i64>(result.first().unwrap_or(&redis::Value::Nil))
                .unwrap_or(0)
                == 1,
            open_until_ms: redis::from_redis_value::<i64>(
                result.get(1).unwrap_or(&redis::Value::Nil),
            )
            .ok()
            .filter(|value| *value > now),
            reason: redis::from_redis_value::<String>(result.get(2).unwrap_or(&redis::Value::Nil))
                .ok()
                .filter(|value| !value.is_empty()),
            recent_failures: redis::from_redis_value::<i64>(
                result.get(3).unwrap_or(&redis::Value::Nil),
            )
            .unwrap_or(0)
            .max(0) as u32,
            distinct_credentials: redis::from_redis_value::<i64>(
                result.get(4).unwrap_or(&redis::Value::Nil),
            )
            .unwrap_or(0)
            .max(0) as u32,
        })
    }

    pub async fn local_pool_circuit_state(
        &self,
        window: StdDuration,
    ) -> anyhow::Result<LocalPoolCircuitState> {
        let now = now_ms();
        let window_ms = window.as_millis().max(1) as i64;
        let mut manager = self.manager.clone();
        let script = r#"
            local now = tonumber(ARGV[1])
            redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now - tonumber(ARGV[2]))

            local members = redis.call('ZRANGE', KEYS[1], 0, -1)
            local failures = 0
            local distinct_credentials = {}
            for _, member in ipairs(members) do
                if string.sub(member, 1, 8) == 'failure:' then
                    failures = failures + 1
                else
                    distinct_credentials[member] = true
                end
            end
            local distinct = 0
            for _, _ in pairs(distinct_credentials) do
                distinct = distinct + 1
            end

            return {
                redis.call('GET', KEYS[2]) or false,
                redis.call('GET', KEYS[3]) or false,
                failures,
                distinct
            }
        "#;
        let result: Vec<redis::Value> = redis::cmd("EVAL")
            .arg(script)
            .arg(3)
            .arg(self.key(local_pool_circuit_failures_key()))
            .arg(self.key(local_pool_circuit_open_until_key()))
            .arg(self.key(local_pool_circuit_reason_key()))
            .arg(now)
            .arg(window_ms)
            .query_async(&mut manager)
            .await?;
        let open_until =
            redis::from_redis_value::<Option<i64>>(result.first().unwrap_or(&redis::Value::Nil))
                .unwrap_or(None);
        let reason =
            redis::from_redis_value::<Option<String>>(result.get(1).unwrap_or(&redis::Value::Nil))
                .unwrap_or(None);
        let recent_failures =
            redis::from_redis_value::<i64>(result.get(2).unwrap_or(&redis::Value::Nil))
                .unwrap_or(0);
        let distinct = redis::from_redis_value::<i64>(result.get(3).unwrap_or(&redis::Value::Nil))
            .unwrap_or(0);
        let open_until_ms = open_until.filter(|until| *until > now);
        if open_until.is_some() && open_until_ms.is_none() {
            let _: () = redis::pipe()
                .cmd("DEL")
                .arg(self.key(local_pool_circuit_open_until_key()))
                .cmd("DEL")
                .arg(self.key(local_pool_circuit_reason_key()))
                .query_async(&mut manager)
                .await
                .unwrap_or(());
        }
        let open = open_until_ms.is_some();
        let reason = if open { reason } else { None };
        Ok(LocalPoolCircuitState {
            open,
            open_until_ms,
            reason,
            recent_failures: recent_failures.max(0) as u32,
            distinct_credentials: distinct.max(0) as u32,
        })
    }

    pub async fn touch_in_flight_lease(
        &self,
        credential_id: u64,
        lease_id: u64,
    ) -> anyhow::Result<()> {
        let keys = in_flight_keys(credential_id);
        let global_keys = global_in_flight_keys();
        let mut manager = self.manager.clone();
        let lease_id = lease_id.to_string();
        let now = now_ms() as f64;
        let _: (i64, i64) = redis::pipe()
            .atomic()
            .cmd("ZADD")
            .arg(self.key(&keys.last_seen))
            .arg(now)
            .arg(&lease_id)
            .cmd("ZADD")
            .arg(self.key(&global_keys.last_seen))
            .arg(now)
            .arg(&lease_id)
            .query_async(&mut manager)
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
        let global_keys = global_in_flight_keys();
        let lease_id = lease_id.to_string();
        let mut manager = self.manager.clone();
        let _: (i64, i64, i64, i64) = redis::pipe()
            .atomic()
            .cmd("HSET")
            .arg(self.key(&keys.kind))
            .arg(&lease_id)
            .arg(kind)
            .cmd("ZADD")
            .arg(self.key(&keys.last_seen))
            .arg(now_ms() as f64)
            .arg(&lease_id)
            .cmd("HSET")
            .arg(self.key(&global_keys.kind))
            .arg(&lease_id)
            .arg(kind)
            .cmd("ZADD")
            .arg(self.key(&global_keys.last_seen))
            .arg(now_ms() as f64)
            .arg(&lease_id)
            .query_async::<(i64, i64, i64, i64)>(&mut manager)
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
                redis.call('ZREM', KEYS[4], member)
                redis.call('ZREM', KEYS[5], member)
                redis.call('HDEL', KEYS[6], member)
            end
            return #expired
        "#;
        let mut manager = self.manager.clone();
        let mut cleaned = 0usize;
        let global_keys = global_in_flight_keys();
        for credential_id in credential_ids {
            let keys = in_flight_keys(*credential_id);
            let removed: i64 = redis::cmd("EVAL")
                .arg(script)
                .arg(6)
                .arg(self.key(&keys.last_seen))
                .arg(self.key(&keys.acquired))
                .arg(self.key(&keys.kind))
                .arg(self.key(&global_keys.last_seen))
                .arg(self.key(&global_keys.acquired))
                .arg(self.key(&global_keys.kind))
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
        let global_keys = global_in_flight_keys();
        let mut manager = self.manager.clone();
        if let Some(min_idle) = min_idle {
            let cutoff = now_ms() - min_idle.as_millis() as i64;
            let script = r#"
                local expired = redis.call('ZRANGEBYSCORE', KEYS[1], '-inf', ARGV[1])
                for _, member in ipairs(expired) do
                    redis.call('ZREM', KEYS[1], member)
                    redis.call('ZREM', KEYS[2], member)
                    redis.call('HDEL', KEYS[3], member)
                    redis.call('ZREM', KEYS[4], member)
                    redis.call('ZREM', KEYS[5], member)
                    redis.call('HDEL', KEYS[6], member)
                end
                return #expired
            "#;
            let removed: i64 = redis::cmd("EVAL")
                .arg(script)
                .arg(6)
                .arg(self.key(&keys.last_seen))
                .arg(self.key(&keys.acquired))
                .arg(self.key(&keys.kind))
                .arg(self.key(&global_keys.last_seen))
                .arg(self.key(&global_keys.acquired))
                .arg(self.key(&global_keys.kind))
                .arg(cutoff)
                .query_async(&mut manager)
                .await?;
            return Ok(removed.max(0) as usize);
        }

        let count: i64 = manager.zcard(self.key(&keys.last_seen)).await.unwrap_or(0);
        let script = r#"
            local leases = redis.call('ZRANGE', KEYS[1], 0, -1)
            for _, member in ipairs(leases) do
                redis.call('ZREM', KEYS[4], member)
                redis.call('ZREM', KEYS[5], member)
                redis.call('HDEL', KEYS[6], member)
            end
            redis.call('DEL', KEYS[1], KEYS[2], KEYS[3])
            return #leases
        "#;
        let _: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(6)
            .arg(self.key(&keys.last_seen))
            .arg(self.key(&keys.acquired))
            .arg(self.key(&keys.kind))
            .arg(self.key(&global_keys.last_seen))
            .arg(self.key(&global_keys.acquired))
            .arg(self.key(&global_keys.kind))
            .query_async(&mut manager)
            .await?;
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

        let query_now = now_ms();
        let mut pipe = redis::pipe();
        for credential_id in credential_ids {
            let keys = in_flight_keys(*credential_id);
            pipe.cmd("GET")
                .arg(self.key(scheduler_cooldown_key(*credential_id)))
                .cmd("GET")
                .arg(self.key(scheduler_health_key(*credential_id)))
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
                .arg(self.key(&keys.kind))
                .cmd("ZCOUNT")
                .arg(self.key(scheduler_selection_window_key(*credential_id)))
                .arg(query_now - 10_000)
                .arg("+inf")
                .cmd("ZCOUNT")
                .arg(self.key(scheduler_selection_window_key(*credential_id)))
                .arg(query_now - 60_000)
                .arg("+inf")
                .cmd("ZCOUNT")
                .arg(self.key(scheduler_selection_window_key(*credential_id)))
                .arg(query_now - 5 * 60_000)
                .arg("+inf")
                .cmd("HGETALL")
                .arg(self.key(scheduler_model_index_key(*credential_id)));
        }

        let mut manager = self.manager.clone();
        let values: Vec<redis::Value> = pipe.query_async(&mut manager).await?;
        let now = now_ms();
        let mut keys_to_delete = Vec::new();
        let mut indexed_models: Vec<(u64, String, String)> = Vec::new();
        for (index, credential_id) in credential_ids.iter().enumerate() {
            let base = index * 10;
            let cooldown_raw: Option<String> = redis::from_redis_value(&values[base])?;
            let health_raw: Option<String> = redis::from_redis_value(&values[base + 1])?;
            let rate_raw: Option<String> = redis::from_redis_value(&values[base + 2])?;
            let last_seen: Vec<(String, f64)> = redis::from_redis_value(&values[base + 3])?;
            let acquired: Vec<(String, f64)> = redis::from_redis_value(&values[base + 4])?;
            let kinds: HashMap<String, String> = redis::from_redis_value(&values[base + 5])?;
            let recent_10s: i64 = redis::from_redis_value(&values[base + 6])?;
            let recent_60s: i64 = redis::from_redis_value(&values[base + 7])?;
            let recent_5m: i64 = redis::from_redis_value(&values[base + 8])?;
            let model_index: HashMap<String, String> = redis::from_redis_value(&values[base + 9])?;
            for (hash, model) in model_index {
                if !hash.is_empty() && !model.trim().is_empty() {
                    indexed_models.push((*credential_id, hash, model));
                }
            }

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
            let mut health = health_raw
                .as_deref()
                .and_then(|raw| serde_json::from_str::<SchedulerHealthState>(raw).ok())
                .unwrap_or_default();
            health.recent_selection_count_10s = recent_10s.max(0).min(u32::MAX as i64) as u32;
            health.recent_selection_count_60s = recent_60s.max(0).min(u32::MAX as i64) as u32;
            health.recent_selection_count_5m = recent_5m.max(0).min(u32::MAX as i64) as u32;
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
                    health,
                    model_states: Vec::new(),
                    rate_limit_available_at_ms,
                    in_flight_leases,
                },
            );
        }
        if !indexed_models.is_empty() {
            let mut model_pipe = redis::pipe();
            for (credential_id, hash, _) in &indexed_models {
                model_pipe
                    .cmd("GET")
                    .arg(self.key(scheduler_model_cooldown_key(*credential_id, hash)))
                    .cmd("GET")
                    .arg(self.key(scheduler_model_health_key(*credential_id, hash)));
            }
            let model_values: Vec<redis::Value> = model_pipe.query_async(&mut manager).await?;
            for (index, (credential_id, hash, model)) in indexed_models.into_iter().enumerate() {
                let base = index * 2;
                let cooldown_raw: Option<String> = redis::from_redis_value(&model_values[base])?;
                let health_raw: Option<String> = redis::from_redis_value(&model_values[base + 1])?;
                let cooldown = cooldown_raw
                    .as_deref()
                    .and_then(|raw| serde_json::from_str::<SchedulerCooldownState>(raw).ok())
                    .and_then(|mut state| {
                        if state.until_ms <= now {
                            keys_to_delete.push(scheduler_model_cooldown_key(credential_id, &hash));
                            None
                        } else {
                            if state.model.is_none() {
                                state.model = Some(model.clone());
                            }
                            Some(state)
                        }
                    });
                let health = health_raw
                    .as_deref()
                    .and_then(|raw| serde_json::from_str::<SchedulerHealthState>(raw).ok())
                    .unwrap_or_default();
                if let Some(state) = states.get_mut(&credential_id) {
                    state.model_states.push(SchedulerModelState {
                        model,
                        cooldown,
                        health,
                    });
                }
            }
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

    pub async fn global_capacity_state(&self) -> anyhow::Result<SchedulerGlobalCapacityState> {
        let keys = global_in_flight_keys();
        let mut manager = self.manager.clone();
        let (in_flight, queued): (i64, Option<i64>) = redis::pipe()
            .cmd("ZCARD")
            .arg(self.key(&keys.last_seen))
            .cmd("GET")
            .arg(self.key(scheduler_global_queue_key()))
            .query_async(&mut manager)
            .await?;
        Ok(SchedulerGlobalCapacityState {
            in_flight_requests: in_flight.max(0) as u32,
            queued_requests: queued.unwrap_or(0).max(0) as u32,
        })
    }

    pub async fn try_enter_dispatch_queue(&self, max_queued: u32) -> anyhow::Result<bool> {
        let script = r#"
            local max_queued = tonumber(ARGV[1])
            local count = tonumber(redis.call('GET', KEYS[1]) or '0')
            if max_queued > 0 and count >= max_queued then
                return 0
            end
            redis.call('INCR', KEYS[1])
            redis.call('EXPIRE', KEYS[1], ARGV[2])
            return 1
        "#;
        let mut manager = self.manager.clone();
        let admitted: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(self.key(scheduler_global_queue_key()))
            .arg(max_queued)
            .arg(3600)
            .query_async(&mut manager)
            .await?;
        Ok(admitted == 1)
    }

    pub async fn leave_dispatch_queue(&self) -> anyhow::Result<()> {
        let script = r#"
            local count = tonumber(redis.call('GET', KEYS[1]) or '0')
            if count <= 1 then
                redis.call('DEL', KEYS[1])
            else
                redis.call('DECR', KEYS[1])
            end
            return 1
        "#;
        let mut manager = self.manager.clone();
        let _: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(self.key(scheduler_global_queue_key()))
            .query_async(&mut manager)
            .await?;
        Ok(())
    }

    pub async fn try_enter_external_pool_dispatch_queue(
        &self,
        max_queued: u32,
    ) -> anyhow::Result<bool> {
        let script = r#"
            local max_queued = tonumber(ARGV[1])
            local count = tonumber(redis.call('GET', KEYS[1]) or '0')
            if max_queued > 0 and count >= max_queued then
                return 0
            end
            redis.call('INCR', KEYS[1])
            redis.call('EXPIRE', KEYS[1], ARGV[2])
            return 1
        "#;
        let mut manager = self.manager.clone();
        let admitted: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(self.key(external_pool_global_queue_key()))
            .arg(max_queued)
            .arg(3600)
            .query_async(&mut manager)
            .await?;
        Ok(admitted == 1)
    }

    pub async fn leave_external_pool_dispatch_queue(&self) -> anyhow::Result<()> {
        let script = r#"
            local count = tonumber(redis.call('GET', KEYS[1]) or '0')
            if count <= 1 then
                redis.call('DEL', KEYS[1])
            else
                redis.call('DECR', KEYS[1])
            end
            return 1
        "#;
        let mut manager = self.manager.clone();
        let _: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(self.key(external_pool_global_queue_key()))
            .query_async(&mut manager)
            .await?;
        Ok(())
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

fn scheduler_health_key(credential_id: u64) -> String {
    format!("scheduler:health:{}", credential_id)
}

fn scheduler_model_hash(model: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(model.trim().to_ascii_lowercase().as_bytes());
    format!("{:x}", hasher.finalize())
}

fn scheduler_model_index_key(credential_id: u64) -> String {
    format!("scheduler:models:{}", credential_id)
}

fn scheduler_model_cooldown_key(credential_id: u64, model_hash: &str) -> String {
    format!("scheduler:cooldown:{}:model:{}", credential_id, model_hash)
}

fn scheduler_model_health_key(credential_id: u64, model_hash: &str) -> String {
    format!("scheduler:health:{}:model:{}", credential_id, model_hash)
}

fn scheduler_selection_window_key(credential_id: u64) -> String {
    format!("scheduler:selection:{}", credential_id)
}

fn scheduler_rate_limit_key(credential_id: u64) -> String {
    format!("scheduler:rate_limit:{}", credential_id)
}

fn scheduler_global_queue_key() -> &'static str {
    "scheduler:global:queued"
}

fn external_pool_global_queue_key() -> &'static str {
    "external_pool:global:queued"
}

fn local_pool_circuit_failures_key() -> &'static str {
    "local_pool:circuit:failures"
}

fn local_pool_circuit_sequence_key() -> &'static str {
    "local_pool:circuit:sequence"
}

fn local_pool_circuit_open_until_key() -> &'static str {
    "local_pool:circuit:open_until"
}

fn local_pool_circuit_reason_key() -> &'static str {
    "local_pool:circuit:reason"
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

fn global_in_flight_keys() -> InFlightKeys {
    InFlightKeys {
        last_seen: "scheduler:global:inflight:last_seen".to_string(),
        acquired: "scheduler:global:inflight:acquired".to_string(),
        kind: "scheduler:global:inflight:kind".to_string(),
    }
}

fn external_pool_in_flight_keys(pool_id: u64) -> InFlightKeys {
    InFlightKeys {
        last_seen: format!("external_pool:inflight:{}:last_seen", pool_id),
        acquired: format!("external_pool:inflight:{}:acquired", pool_id),
        kind: format!("external_pool:inflight:{}:kind", pool_id),
    }
}

fn external_pool_global_in_flight_keys() -> InFlightKeys {
    InFlightKeys {
        last_seen: "external_pool:global:inflight:last_seen".to_string(),
        acquired: "external_pool:global:inflight:acquired".to_string(),
        kind: "external_pool:global:inflight:kind".to_string(),
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
        store.clear_scheduler_cooldown(3).await.unwrap();
        let cooldown = store
            .set_scheduler_cooldown(3, StdDuration::from_secs(30), Some("429".to_string()))
            .await
            .unwrap();
        assert!(cooldown.until_ms > now_ms());
        let loaded = store.get_scheduler_cooldown(3).await.unwrap().unwrap();
        assert_eq!(loaded.reason.as_deref(), Some("429"));
        let shorter = store
            .set_scheduler_cooldown(3, StdDuration::from_secs(1), Some("short".to_string()))
            .await
            .unwrap();
        assert_eq!(shorter, cooldown);
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

        let (_, failure_health) = store
            .record_scheduler_transient_failure(
                3,
                None,
                "rate_limit",
                "429",
                None,
                StdDuration::from_secs(1),
                StdDuration::from_secs(30),
                2.0,
                1.0,
                StdDuration::from_secs(5),
                0.2,
            )
            .await
            .unwrap();
        assert_eq!(failure_health.transient_failure_streak, 1);
        assert!(failure_health.recent_error_rate > 0.0);
        let success_health = store
            .record_scheduler_success(3, None, Some(StdDuration::from_millis(120)), 0.2)
            .await
            .unwrap();
        assert_eq!(success_health.transient_failure_streak, 0);
        assert_eq!(success_health.latency_ewma_ms, Some(120.0));

        store.clear_scheduler_health(4).await.unwrap();
        store.clear_scheduler_cooldown(4).await.unwrap();
        let (model_cooldown, model_failure_health) = store
            .record_scheduler_transient_failure(
                4,
                Some("claude-opus-4.8"),
                "rate_limit",
                "429 opus",
                Some(StdDuration::from_secs(10)),
                StdDuration::from_secs(1),
                StdDuration::from_secs(30),
                2.0,
                1.0,
                StdDuration::from_secs(5),
                0.2,
            )
            .await
            .unwrap();
        assert_eq!(model_cooldown.model.as_deref(), Some("claude-opus-4.8"));
        assert_eq!(model_failure_health.transient_failure_streak, 1);
        let model_states = store.scheduler_state_for_credentials(&[4]).await.unwrap();
        let credential_state = model_states.get(&4).unwrap();
        assert!(credential_state.cooldown.is_none());
        let opus_state = credential_state
            .model_states
            .iter()
            .find(|state| state.model == "claude-opus-4.8")
            .unwrap();
        assert_eq!(
            opus_state
                .cooldown
                .as_ref()
                .and_then(|cooldown| cooldown.reason.as_deref()),
            Some("429 opus")
        );
        let model_success_health = store
            .record_scheduler_success(
                4,
                Some("claude-opus-4.8"),
                Some(StdDuration::from_millis(88)),
                0.2,
            )
            .await
            .unwrap();
        assert_eq!(model_success_health.transient_failure_streak, 0);
        assert!(store.get_scheduler_cooldown(4).await.unwrap().is_none());

        let selected_once = store.record_scheduler_selection(3).await.unwrap();
        assert_eq!(selected_once.selection_count, 1);
        assert_eq!(selected_once.recent_selection_count_10s, 1);
        assert_eq!(selected_once.recent_selection_count_60s, 1);
        assert_eq!(selected_once.recent_selection_count_5m, 1);
        let selected_twice = store.record_scheduler_selection(3).await.unwrap();
        assert_eq!(selected_twice.selection_count, 2);
        assert_eq!(
            store
                .scheduler_state_for_credentials(&[3])
                .await
                .unwrap()
                .get(&3)
                .unwrap()
                .health
                .recent_selection_count_60s,
            2
        );
        store.clear_scheduler_health(3).await.unwrap();
        store.clear_scheduler_health(4).await.unwrap();
    }

    #[tokio::test]
    async fn redis_local_pool_circuit_uses_sliding_window_and_distinct_credentials() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let window = StdDuration::from_millis(80);
        let open_for = StdDuration::from_secs(2);

        let state = store.local_pool_circuit_state(window).await.unwrap();
        assert!(!state.open);
        assert_eq!(state.recent_failures, 0);
        assert_eq!(state.distinct_credentials, 0);

        let state = store
            .record_local_pool_circuit_failure(
                Some(1),
                "local_transient_exhausted",
                window,
                3,
                2,
                open_for,
            )
            .await
            .unwrap();
        assert!(!state.open);
        assert_eq!(state.recent_failures, 1);
        assert_eq!(state.distinct_credentials, 1);
        assert_eq!(state.reason, None);

        let state = store
            .record_local_pool_circuit_failure(
                Some(1),
                "local_transient_exhausted",
                window,
                3,
                2,
                open_for,
            )
            .await
            .unwrap();
        assert!(!state.open);
        assert_eq!(state.recent_failures, 2);
        assert_eq!(state.distinct_credentials, 1);

        let state = store
            .record_local_pool_circuit_failure(
                Some(2),
                "local_transient_exhausted",
                window,
                3,
                2,
                open_for,
            )
            .await
            .unwrap();
        assert!(state.open);
        assert!(state.open_until_ms.is_some());
        assert_eq!(state.recent_failures, 3);
        assert_eq!(state.distinct_credentials, 2);
        assert_eq!(state.reason.as_deref(), Some("local_transient_exhausted"));

        tokio::time::sleep(StdDuration::from_millis(110)).await;
        let state = store.local_pool_circuit_state(window).await.unwrap();
        assert!(state.open);
        assert_eq!(state.recent_failures, 0);
        assert_eq!(state.distinct_credentials, 0);
        assert_eq!(state.reason.as_deref(), Some("local_transient_exhausted"));
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
        assert_eq!(
            store
                .global_capacity_state()
                .await
                .unwrap()
                .in_flight_requests,
            1
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
        assert_eq!(
            store
                .global_capacity_state()
                .await
                .unwrap()
                .in_flight_requests,
            0
        );
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
