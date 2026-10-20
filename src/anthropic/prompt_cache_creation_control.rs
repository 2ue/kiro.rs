use std::collections::{HashMap, VecDeque, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};

use chrono::{DateTime, Utc};
use parking_lot::Mutex;

use crate::anthropic::cache::CacheUsage;
use crate::anthropic::prompt_cache::PromptCacheScope;
use crate::model::config::{PromptCacheCreationControlConfig, PromptCacheCreationControlScopeMode};

#[derive(Debug, Default)]
pub struct PromptCacheCreationController {
    states: Mutex<HashMap<CreationControlKey, CreationControlState>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CreationControlKey {
    conversation_id: String,
    route_namespace: Option<String>,
    credential_key: Option<String>,
    model: Option<String>,
}

impl CreationControlKey {
    fn from_scope(
        scope: &PromptCacheScope,
        mode: PromptCacheCreationControlScopeMode,
        credential_key: Option<&str>,
        model: Option<&str>,
    ) -> Self {
        let credential_key = credential_key
            .map(str::trim)
            .filter(|credential_key| !credential_key.is_empty())
            .map(str::to_string);
        let model = model
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(str::to_string);
        let credential_key = match mode {
            PromptCacheCreationControlScopeMode::CredentialConversationModel => credential_key,
            PromptCacheCreationControlScopeMode::ConversationModel => None,
        };
        Self {
            conversation_id: scope.conversation_id.clone(),
            route_namespace: scope.route_namespace.clone(),
            credential_key,
            model,
        }
    }
}

#[derive(Debug, Default, Clone)]
struct CreationControlState {
    last_seen_at: Option<DateTime<Utc>>,
    last_creation_at: Option<DateTime<Utc>>,
    successful_requests_since_creation: u32,
    pending_creation_tokens: i32,
    window_events: VecDeque<CreationWindowEvent>,
    /// 命中率整形在范围内没能补上的计费差额（美元），留到之后放行写入的请求上补。
    cost_deficit_usd: f64,
}

#[derive(Debug, Clone, Copy)]
struct CreationWindowEvent {
    at: DateTime<Utc>,
    tokens: i32,
}

impl PromptCacheCreationController {
    #[cfg(test)]
    pub fn preview_success(
        &self,
        scope: Option<&PromptCacheScope>,
        config: PromptCacheCreationControlConfig,
        usage: CacheUsage,
    ) -> CacheUsage {
        self.preview_success_with_context(scope, config, usage, None, None)
    }

    pub fn preview_success_with_context(
        &self,
        scope: Option<&PromptCacheScope>,
        config: PromptCacheCreationControlConfig,
        usage: CacheUsage,
        credential_key: Option<&str>,
        model: Option<&str>,
    ) -> CacheUsage {
        self.preview_success_with_mode(scope, config, usage, credential_key, model, false)
    }

    /// `stable_window` 为 true 时（命中率整形开启）使用修正后的窗口额度：抖动只作用于
    /// 窗口总额度，部分放行低于 1024 时整笔延后；为 false 时保持原有逻辑。
    pub fn preview_success_with_mode(
        &self,
        scope: Option<&PromptCacheScope>,
        config: PromptCacheCreationControlConfig,
        usage: CacheUsage,
        credential_key: Option<&str>,
        model: Option<&str>,
        stable_window: bool,
    ) -> CacheUsage {
        let config = config.normalized();
        if !config.enabled {
            return usage;
        }
        let Some(scope) = scope else {
            return usage;
        };

        let creation = usage.cache_creation_input_tokens.max(0);
        if creation <= 0 {
            return usage;
        }

        let now = Utc::now();
        let key = CreationControlKey::from_scope(scope, config.scope_mode, credential_key, model);
        let mut states = self.states.lock();
        prune_idle_states(&mut states, now, config);
        let mut state = states.get(&key).cloned().unwrap_or_default();
        prune_window_events(&mut state, now, config);
        let allowed_creation =
            allowed_creation_tokens(&key, &state, config, usage, creation, now, stable_window);
        with_allowed_creation(usage, allowed_creation)
    }

    #[cfg(test)]
    pub fn apply_success(
        &self,
        scope: Option<&PromptCacheScope>,
        config: PromptCacheCreationControlConfig,
        usage: CacheUsage,
    ) -> CacheUsage {
        self.apply_success_with_context(scope, config, usage, None, None)
    }

    pub fn apply_success_with_context(
        &self,
        scope: Option<&PromptCacheScope>,
        config: PromptCacheCreationControlConfig,
        usage: CacheUsage,
        credential_key: Option<&str>,
        model: Option<&str>,
    ) -> CacheUsage {
        self.apply_success_with_mode(scope, config, usage, credential_key, model, false)
    }

    /// `stable_window` 含义同 [`Self::preview_success_with_mode`]。
    pub fn apply_success_with_mode(
        &self,
        scope: Option<&PromptCacheScope>,
        config: PromptCacheCreationControlConfig,
        usage: CacheUsage,
        credential_key: Option<&str>,
        model: Option<&str>,
        stable_window: bool,
    ) -> CacheUsage {
        let config = config.normalized();
        if !config.enabled {
            return usage;
        }
        let Some(scope) = scope else {
            return usage;
        };

        let now = Utc::now();
        let creation = usage.cache_creation_input_tokens.max(0);
        let mut states = self.states.lock();
        prune_idle_states(&mut states, now, config);

        let key = CreationControlKey::from_scope(scope, config.scope_mode, credential_key, model);
        if creation <= 0 {
            if let Some(state) = states.get_mut(&key) {
                prune_window_events(state, now, config);
                state.last_seen_at = Some(now);
                state.successful_requests_since_creation =
                    state.successful_requests_since_creation.saturating_add(1);
            }
            return usage;
        }

        let state = states.entry(key.clone()).or_default();
        prune_window_events(state, now, config);
        let allowed_creation =
            allowed_creation_tokens(&key, state, config, usage, creation, now, stable_window);
        let adjusted = with_allowed_creation(usage, allowed_creation);

        state.last_seen_at = Some(now);
        if allowed_creation > 0 {
            state.last_creation_at = Some(now);
            state.successful_requests_since_creation = 0;
            state.pending_creation_tokens = 0;
            state.window_events.push_back(CreationWindowEvent {
                at: now,
                tokens: allowed_creation,
            });
        } else {
            state.successful_requests_since_creation =
                state.successful_requests_since_creation.saturating_add(1);
        }

        if allowed_creation < creation {
            state.pending_creation_tokens = state
                .pending_creation_tokens
                .saturating_add(creation.saturating_sub(allowed_creation));
        }

        adjusted
    }

    /// 最终整形改变了上报的写入量时，把差额记入同一控制维度的窗口额度，
    /// 让频次与额度按下游真正看到的写入计算。只调整已经放行写入的请求。
    pub fn record_final_creation_adjustment(
        &self,
        scope: Option<&PromptCacheScope>,
        config: PromptCacheCreationControlConfig,
        credential_key: Option<&str>,
        model: Option<&str>,
        controlled_creation: i32,
        final_creation: i32,
    ) {
        let config = config.normalized();
        if !config.enabled || controlled_creation <= 0 {
            return;
        }
        let Some(scope) = scope else {
            return;
        };
        let delta = final_creation
            .max(0)
            .saturating_sub(controlled_creation.max(0));
        if delta == 0 {
            return;
        }
        let key = CreationControlKey::from_scope(scope, config.scope_mode, credential_key, model);
        let mut states = self.states.lock();
        let Some(state) = states.get_mut(&key) else {
            return;
        };
        if let Some(event) = state.window_events.back_mut() {
            event.tokens = event.tokens.saturating_add(delta).max(0);
        }
    }

    /// 同一控制维度此前累计、尚未补回的计费差额（美元）。
    ///
    /// 只读单个条目，不做全表过期清理（清理由同一请求的 `apply_success_with_mode` 负责）；
    /// 条目已空闲过期时视为 0。
    pub fn cost_deficit_usd(
        &self,
        scope: Option<&PromptCacheScope>,
        config: PromptCacheCreationControlConfig,
        credential_key: Option<&str>,
        model: Option<&str>,
    ) -> f64 {
        let config = config.normalized();
        if !tracks_cost_deficit(config) {
            return 0.0;
        }
        let Some(scope) = scope else {
            return 0.0;
        };
        let key = CreationControlKey::from_scope(scope, config.scope_mode, credential_key, model);
        let now = Utc::now();
        self.states
            .lock()
            .get(&key)
            .filter(|state| !is_idle_expired(state, now, config))
            .map(|state| state.cost_deficit_usd.max(0.0))
            .unwrap_or(0.0)
    }

    /// 记录一次最终下游计费相对计费下限的差额：低于下限的部分累加，高于下限的部分抵扣。
    ///
    /// - 只在 creation control 开启且设置了空闲过期时记账：差额只在放行写入时补，没有频次
    ///   控制时每次都放行，不需要跨请求累计；没有空闲过期时条目无法回收。
    /// - 没有差额时不新建条目；累计额不超过本次下限的 [`MAX_CARRIED_DEFICIT_FLOOR_MULTIPLE`] 倍，
    ///   但不会因为本次下限较小而把此前已累计的差额截掉。
    /// - 条目已空闲过期时视为新会话：整条状态重置，不续期旧的频次 / 窗口状态。
    /// - 不做全表过期清理，避免每个请求多扫一遍状态表。
    pub fn record_cost_outcome(
        &self,
        scope: Option<&PromptCacheScope>,
        config: PromptCacheCreationControlConfig,
        credential_key: Option<&str>,
        model: Option<&str>,
        floor_usd: f64,
        actual_usd: f64,
    ) {
        if !floor_usd.is_finite() || !actual_usd.is_finite() || floor_usd <= 0.0 {
            return;
        }
        let config = config.normalized();
        if !tracks_cost_deficit(config) {
            return;
        }
        let Some(scope) = scope else {
            return;
        };
        let now = Utc::now();
        let key = CreationControlKey::from_scope(scope, config.scope_mode, credential_key, model);
        let mut states = self.states.lock();
        if states
            .get(&key)
            .is_some_and(|state| is_idle_expired(state, now, config))
        {
            states.remove(&key);
        }
        let previous = states
            .get(&key)
            .map(|state| state.cost_deficit_usd.max(0.0))
            .unwrap_or(0.0);
        let limit = (floor_usd * MAX_CARRIED_DEFICIT_FLOOR_MULTIPLE).max(previous);
        let deficit = (previous + floor_usd - actual_usd).clamp(0.0, limit);
        match states.get_mut(&key) {
            Some(state) => {
                state.cost_deficit_usd = deficit;
                state.last_seen_at = Some(now);
            }
            None if deficit > 0.0 => {
                states.insert(
                    key,
                    CreationControlState {
                        last_seen_at: Some(now),
                        cost_deficit_usd: deficit,
                        ..CreationControlState::default()
                    },
                );
            }
            None => {}
        }
    }

    /// 当前状态表条目数（诊断与测试用）。
    #[cfg(test)]
    pub(crate) fn state_count(&self) -> usize {
        self.states.lock().len()
    }

    pub fn clear_credential(&self, credential_id: u64) {
        let credential_key = format!("credential:{credential_id}");
        self.states.lock().retain(|key, _| {
            key.credential_key
                .as_deref()
                .is_none_or(|key| key != credential_key)
        });
    }
}

fn allowed_creation_tokens(
    key: &CreationControlKey,
    state: &CreationControlState,
    config: PromptCacheCreationControlConfig,
    usage: CacheUsage,
    creation: i32,
    now: DateTime<Utc>,
    stable_window: bool,
) -> i32 {
    let mut allowed = creation.max(0);
    if allowed <= 0 {
        return 0;
    }

    let first_creation = state.last_creation_at.is_none();
    if !first_creation {
        if state.successful_requests_since_creation
            < config.min_successful_requests_between_creation
        {
            allowed = 0;
        }

        if allowed > 0 && config.min_creation_interval_secs > 0 {
            if let Some(last_creation_at) = state.last_creation_at {
                let elapsed = now.signed_duration_since(last_creation_at).num_seconds();
                if elapsed < config.min_creation_interval_secs as i64 {
                    allowed = 0;
                }
            }
        }

        if allowed > 0 && config.min_creation_delta_tokens > 0 {
            let pending_total = state
                .pending_creation_tokens
                .saturating_add(creation)
                .max(0);
            if pending_total < config.min_creation_delta_tokens {
                allowed = 0;
            }
        }
    }

    if allowed <= 0 {
        return 0;
    }

    if config.max_creation_tokens_per_event > 0 {
        let cap = jittered_creation_limit(
            config.max_creation_tokens_per_event,
            creation_cap_seed(key, state, config, usage, creation, 0x9b6d_9f43_21a4_0f17),
        );
        allowed = allowed.min(cap);
    }

    if config.creation_budget_window_secs > 0 && config.max_creation_tokens_per_window > 0 {
        let used = state
            .window_events
            .iter()
            .map(|event| event.tokens.max(0))
            .fold(0_i32, i32::saturating_add);
        let remaining = config
            .max_creation_tokens_per_window
            .saturating_sub(used)
            .max(0);
        let seed = creation_cap_seed(key, state, config, usage, creation, 0xc2a4_5d13_8e9f_62b7);
        let remaining = if stable_window {
            // 抖动只作用于窗口总额度，不作用于剩余额度：对剩余额度反复取 88%~97% 会让
            // 额度几何衰减，最终放行 179、6、1 这类不可能出现的写入值。
            let window_cap = jittered_creation_limit(config.max_creation_tokens_per_window, seed);
            remaining.min(window_cap.saturating_sub(used).max(0))
        } else {
            jittered_creation_limit(remaining, seed)
        };
        allowed = allowed.min(remaining);
    }

    // 部分放行时，余量低于最小可缓存长度就整笔延后，不上报碎片写入。
    if stable_window && allowed < creation && allowed < MIN_PARTIAL_CREATION_TOKENS {
        return 0;
    }

    allowed.max(0)
}

/// 累计计费差额的上限：本次计费下限的倍数。
const MAX_CARRIED_DEFICIT_FLOOR_MULTIPLE: f64 = 20.0;

/// 部分放行写入的最小值，与 Anthropic 最小可缓存长度（1024）一致。
const MIN_PARTIAL_CREATION_TOKENS: i32 = 1024;

fn jittered_creation_limit(max_tokens: i32, seed: u64) -> i32 {
    let max_tokens = max_tokens.max(0);
    if max_tokens <= 1 {
        return max_tokens;
    }

    let min_jitter = (max_tokens / 33).max(1).min(max_tokens - 1);
    let max_jitter = ((max_tokens as i64 * 12 / 100) as i32)
        .max(min_jitter)
        .min(max_tokens - 1);
    max_tokens.saturating_sub(sample_inclusive(seed, min_jitter, max_jitter))
}

fn sample_inclusive(seed: u64, low: i32, high: i32) -> i32 {
    let low = low.max(0);
    let high = high.max(low);
    let span = (high - low + 1) as u64;
    low + (splitmix64(seed) % span) as i32
}

fn creation_cap_seed(
    key: &CreationControlKey,
    state: &CreationControlState,
    config: PromptCacheCreationControlConfig,
    usage: CacheUsage,
    creation: i32,
    salt: u64,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    state.successful_requests_since_creation.hash(&mut hasher);
    state.pending_creation_tokens.hash(&mut hasher);
    state.window_events.len().hash(&mut hasher);
    state
        .window_events
        .iter()
        .map(|event| event.tokens.max(0))
        .fold(0_i32, i32::saturating_add)
        .hash(&mut hasher);
    config.max_creation_tokens_per_event.hash(&mut hasher);
    config.max_creation_tokens_per_window.hash(&mut hasher);
    usage.input_tokens.hash(&mut hasher);
    usage.output_tokens.hash(&mut hasher);
    usage.cache_read_input_tokens.hash(&mut hasher);
    creation.hash(&mut hasher);
    splitmix64(hasher.finish() ^ salt)
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn with_allowed_creation(usage: CacheUsage, allowed_creation: i32) -> CacheUsage {
    let original_creation = usage.cache_creation_input_tokens.max(0);
    let allowed_creation = allowed_creation.max(0).min(original_creation);
    if allowed_creation == original_creation {
        return usage;
    }

    let suppressed_creation = original_creation.saturating_sub(allowed_creation);
    let (cache_creation_5m_input_tokens, cache_creation_1h_input_tokens) = cap_creation_breakdown(
        usage.cache_creation_5m_input_tokens,
        usage.cache_creation_1h_input_tokens,
        allowed_creation,
    );
    let input_tokens = usage.input_tokens.saturating_add(suppressed_creation);

    CacheUsage {
        input_tokens,
        cache_creation_input_tokens: allowed_creation,
        cache_creation_5m_input_tokens,
        cache_creation_1h_input_tokens,
        total_input_tokens: reported_total_input_tokens(
            input_tokens,
            usage.cache_read_input_tokens,
            allowed_creation,
        ),
        ..usage
    }
}

fn cap_creation_breakdown(cache5m: i32, cache1h: i32, limit: i32) -> (i32, i32) {
    let limit = limit.max(0);
    let cache5m = cache5m.max(0).min(limit);
    let cache1h = cache1h.max(0).min(limit.saturating_sub(cache5m));
    (cache5m, cache1h)
}

fn reported_total_input_tokens(
    input_tokens: i32,
    cache_read_tokens: i32,
    creation_tokens: i32,
) -> i32 {
    input_tokens
        .max(0)
        .saturating_add(cache_read_tokens.max(0))
        .saturating_add(creation_tokens.max(0))
}

fn prune_window_events(
    state: &mut CreationControlState,
    now: DateTime<Utc>,
    config: PromptCacheCreationControlConfig,
) {
    if config.creation_budget_window_secs == 0 {
        state.window_events.clear();
        return;
    }
    while state.window_events.front().is_some_and(|event| {
        now.signed_duration_since(event.at).num_seconds()
            >= config.creation_budget_window_secs as i64
    }) {
        state.window_events.pop_front();
    }
}

fn prune_idle_states(
    states: &mut HashMap<CreationControlKey, CreationControlState>,
    now: DateTime<Utc>,
    config: PromptCacheCreationControlConfig,
) {
    if config.expire_after_idle_secs == 0 {
        return;
    }
    states.retain(|_, state| !is_idle_expired(state, now, config));
}

fn is_idle_expired(
    state: &CreationControlState,
    now: DateTime<Utc>,
    config: PromptCacheCreationControlConfig,
) -> bool {
    config.expire_after_idle_secs > 0
        && state.last_seen_at.is_some_and(|last_seen_at| {
            now.signed_duration_since(last_seen_at).num_seconds()
                >= config.expire_after_idle_secs as i64
        })
}

/// 计费差额只在 creation control 开启且有空闲过期时跨请求累计。
fn tracks_cost_deficit(config: PromptCacheCreationControlConfig) -> bool {
    config.enabled && config.expire_after_idle_secs > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> PromptCacheScope {
        PromptCacheScope::new("conversation-a".to_string(), None)
    }

    fn usage(creation: i32) -> CacheUsage {
        CacheUsage {
            total_input_tokens: 10_000 + creation,
            input_tokens: 10_000,
            output_tokens: 100,
            cache_creation_input_tokens: creation,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: creation,
            cache_creation_1h_input_tokens: 0,
        }
    }

    fn enabled_config() -> PromptCacheCreationControlConfig {
        PromptCacheCreationControlConfig {
            enabled: true,
            scope_mode: PromptCacheCreationControlScopeMode::CredentialConversationModel,
            min_successful_requests_between_creation: 2,
            min_creation_interval_secs: 0,
            min_creation_delta_tokens: 5_000,
            max_creation_tokens_per_event: 30_000,
            creation_budget_window_secs: 300,
            max_creation_tokens_per_window: 90_000,
            expire_after_idle_secs: 3_600,
        }
    }

    #[test]
    fn disabled_controller_preserves_usage() {
        let controller = PromptCacheCreationController::default();
        let config = PromptCacheCreationControlConfig {
            enabled: false,
            ..PromptCacheCreationControlConfig::default()
        };
        let original = usage(20_000);

        let adjusted = controller.apply_success(Some(&scope()), config, original);

        assert_eq!(adjusted, original);
    }

    #[test]
    fn first_creation_is_allowed_but_immediate_next_creation_is_suppressed() {
        let controller = PromptCacheCreationController::default();
        let config = enabled_config();
        let scope = scope();

        let first = controller.apply_success(Some(&scope), config, usage(20_000));
        assert_eq!(first.cache_creation_input_tokens, 20_000);
        assert_eq!(first.input_tokens, 10_000);

        let second = controller.apply_success(Some(&scope), config, usage(12_000));
        assert_eq!(second.cache_creation_input_tokens, 0);
        assert_eq!(second.cache_creation_5m_input_tokens, 0);
        assert_eq!(second.input_tokens, 22_000);
        assert_eq!(second.total_input_tokens, 22_000);
    }

    #[test]
    fn default_config_controls_immediate_repeated_creation() {
        let controller = PromptCacheCreationController::default();
        let config = PromptCacheCreationControlConfig::default();
        let scope = scope();

        let first = controller.apply_success(Some(&scope), config, usage(20_000));
        let second = controller.apply_success(Some(&scope), config, usage(12_000));

        assert!(config.enabled);
        assert_eq!(first.cache_creation_input_tokens, 20_000);
        assert_eq!(second.cache_creation_input_tokens, 0);
        assert_eq!(second.input_tokens, 22_000);
    }

    #[test]
    fn creation_is_allowed_after_enough_successful_requests() {
        let controller = PromptCacheCreationController::default();
        let config = enabled_config();
        let scope = scope();

        controller.apply_success(Some(&scope), config, usage(20_000));
        controller.apply_success(Some(&scope), config, usage(0));
        controller.apply_success(Some(&scope), config, usage(0));

        let adjusted = controller.apply_success(Some(&scope), config, usage(12_000));

        assert_eq!(adjusted.cache_creation_input_tokens, 12_000);
        assert_eq!(adjusted.input_tokens, 10_000);
    }

    #[test]
    fn creation_event_cap_is_jittered_below_configured_max() {
        let controller = PromptCacheCreationController::default();
        let config = enabled_config();
        let scope = scope();

        let adjusted = controller.apply_success(Some(&scope), config, usage(80_000));

        assert!((26_400..30_000).contains(&adjusted.cache_creation_input_tokens));
        assert_eq!(
            adjusted.input_tokens,
            90_000 - adjusted.cache_creation_input_tokens
        );
        assert_eq!(adjusted.total_input_tokens, 90_000);
    }

    #[test]
    fn creation_event_cap_does_not_emit_configured_threshold_across_many_scopes() {
        let controller = PromptCacheCreationController::default();
        let mut config = enabled_config();
        config.min_successful_requests_between_creation = 0;
        config.min_creation_delta_tokens = 0;
        config.creation_budget_window_secs = 0;
        let values: Vec<i32> = (0..96)
            .map(|idx| {
                let scope = PromptCacheScope::new(format!("conversation-{idx}"), None);
                controller
                    .apply_success_with_context(
                        Some(&scope),
                        config,
                        usage(80_000),
                        Some(&format!("credential:{}", idx % 7)),
                        Some(if idx % 2 == 0 {
                            "claude-sonnet-4.5"
                        } else {
                            "claude-opus-4.5"
                        }),
                    )
                    .cache_creation_input_tokens
            })
            .collect();

        assert!(values.iter().all(|value| (26_400..30_000).contains(value)));
        assert!(values.iter().all(|value| *value != 30_000));
        let min_value = values.iter().copied().min().unwrap_or_default();
        let max_value = values.iter().copied().max().unwrap_or_default();
        let unique = values
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>()
            .len();
        println!(
            "CACHE_SWEEP creation_control scopes=96 configured_event_cap=30000 exact_cap_hits=0 min_creation={min_value} max_creation={max_value} unique_creation_values={unique}"
        );
        assert!(unique >= 24, "jitter should produce varied caps: {unique}");
    }

    #[test]
    fn creation_window_budget_limits_total_allowed_creation() {
        let controller = PromptCacheCreationController::default();
        let mut config = enabled_config();
        config.min_successful_requests_between_creation = 0;
        config.min_creation_delta_tokens = 0;
        config.max_creation_tokens_per_event = 0;
        config.max_creation_tokens_per_window = 45_000;
        let scope = scope();

        let first = controller.apply_success(Some(&scope), config, usage(30_000));
        let second = controller.apply_success(Some(&scope), config, usage(30_000));

        assert_eq!(first.cache_creation_input_tokens, 30_000);
        assert!((13_200..15_000).contains(&second.cache_creation_input_tokens));
        assert_eq!(
            second.input_tokens,
            40_000 - second.cache_creation_input_tokens
        );
        assert_eq!(second.total_input_tokens, 40_000);
    }

    #[test]
    fn conversation_model_scope_shares_frequency_state_across_credentials() {
        let controller = PromptCacheCreationController::default();
        let mut config = enabled_config();
        config.scope_mode = PromptCacheCreationControlScopeMode::ConversationModel;
        let first_scope = scope();
        let second_scope = scope();

        let first = controller.apply_success(Some(&first_scope), config, usage(20_000));
        let second = controller.apply_success(Some(&second_scope), config, usage(12_000));

        assert_eq!(first.cache_creation_input_tokens, 20_000);
        assert_eq!(second.cache_creation_input_tokens, 0);
        assert_eq!(second.input_tokens, 22_000);
    }

    #[test]
    fn legacy_credential_conversation_model_scope_is_session_scoped() {
        let controller = PromptCacheCreationController::default();
        let config = enabled_config();
        let first_scope = scope();
        let second_scope = scope();

        let first = controller.apply_success(Some(&first_scope), config, usage(20_000));
        let second = controller.apply_success(Some(&second_scope), config, usage(12_000));

        assert_eq!(first.cache_creation_input_tokens, 20_000);
        assert_eq!(second.cache_creation_input_tokens, 0);
        assert_eq!(second.input_tokens, 22_000);
    }

    #[test]
    fn credential_conversation_model_scope_is_credential_and_model_scoped() {
        let controller = PromptCacheCreationController::default();
        let mut config = enabled_config();
        config.min_successful_requests_between_creation = 10;
        config.min_creation_delta_tokens = 0;
        let scope = scope();

        let first = controller.apply_success_with_context(
            Some(&scope),
            config,
            usage(20_000),
            Some("credential:1"),
            Some("claude-sonnet-4.5"),
        );
        let second_credential = controller.apply_success_with_context(
            Some(&scope),
            config,
            usage(12_000),
            Some("credential:2"),
            Some("claude-sonnet-4.5"),
        );
        let repeated_first = controller.apply_success_with_context(
            Some(&scope),
            config,
            usage(12_000),
            Some("credential:1"),
            Some("claude-sonnet-4.5"),
        );
        let other_model = controller.apply_success_with_context(
            Some(&scope),
            config,
            usage(14_000),
            Some("credential:1"),
            Some("claude-opus-4.5"),
        );

        assert_eq!(first.cache_creation_input_tokens, 20_000);
        assert_eq!(second_credential.cache_creation_input_tokens, 12_000);
        assert_eq!(repeated_first.cache_creation_input_tokens, 0);
        assert_eq!(other_model.cache_creation_input_tokens, 14_000);
    }

    #[test]
    fn conversation_model_scope_shares_credentials_but_not_models() {
        let controller = PromptCacheCreationController::default();
        let mut config = enabled_config();
        config.scope_mode = PromptCacheCreationControlScopeMode::ConversationModel;
        config.min_successful_requests_between_creation = 10;
        config.min_creation_delta_tokens = 0;
        let scope = scope();

        let first = controller.apply_success_with_context(
            Some(&scope),
            config,
            usage(20_000),
            Some("credential:1"),
            Some("claude-sonnet-4.5"),
        );
        let other_credential_same_model = controller.apply_success_with_context(
            Some(&scope),
            config,
            usage(12_000),
            Some("credential:2"),
            Some("claude-sonnet-4.5"),
        );
        let other_model = controller.apply_success_with_context(
            Some(&scope),
            config,
            usage(14_000),
            Some("credential:2"),
            Some("claude-opus-4.5"),
        );

        assert_eq!(first.cache_creation_input_tokens, 20_000);
        assert_eq!(other_credential_same_model.cache_creation_input_tokens, 0);
        assert_eq!(other_model.cache_creation_input_tokens, 14_000);
    }

    #[test]
    fn route_namespace_keeps_creation_frequency_state_independent() {
        let controller = PromptCacheCreationController::default();
        let config = enabled_config();
        let mut first_scope = scope();
        first_scope.route_namespace = Some("/dfcache/a".to_string());
        let mut second_scope = scope();
        second_scope.route_namespace = Some("/dfcache/b".to_string());

        let first = controller.apply_success(Some(&first_scope), config, usage(20_000));
        let second = controller.apply_success(Some(&second_scope), config, usage(12_000));

        assert_eq!(first.cache_creation_input_tokens, 20_000);
        assert_eq!(second.cache_creation_input_tokens, 12_000);
    }

    #[test]
    fn preview_prunes_idle_state_like_apply_path() {
        let controller = PromptCacheCreationController::default();
        let mut config = enabled_config();
        config.expire_after_idle_secs = 1;
        config.min_successful_requests_between_creation = 10;
        let scope = scope();

        let first = controller.apply_success(Some(&scope), config, usage(20_000));
        assert_eq!(first.cache_creation_input_tokens, 20_000);

        {
            let key = CreationControlKey::from_scope(&scope, config.scope_mode, None, None);
            let mut states = controller.states.lock();
            let state = states.get_mut(&key).expect("state after first creation");
            state.last_seen_at = Some(Utc::now() - chrono::Duration::seconds(2));
        }

        let previewed = controller.preview_success(Some(&scope), config, usage(12_000));
        let applied = controller.apply_success(Some(&scope), config, usage(12_000));

        assert_eq!(previewed.cache_creation_input_tokens, 12_000);
        assert_eq!(applied.cache_creation_input_tokens, 12_000);
    }

    #[test]
    fn cost_outcome_accumulates_deficit_and_repays_surplus() {
        let controller = PromptCacheCreationController::default();
        let config = enabled_config();
        let scope = scope();
        let read = |controller: &PromptCacheCreationController| {
            controller.cost_deficit_usd(Some(&scope), config, Some("credential:1"), Some("m"))
        };
        assert_eq!(read(&controller), 0.0);

        controller.record_cost_outcome(
            Some(&scope),
            config,
            Some("credential:1"),
            Some("m"),
            0.05,
            0.02,
        );
        controller.record_cost_outcome(
            Some(&scope),
            config,
            Some("credential:1"),
            Some("m"),
            0.05,
            0.03,
        );
        assert!((read(&controller) - 0.05).abs() < 1e-12);

        // 其他凭据维度互不影响。
        assert_eq!(
            controller.cost_deficit_usd(Some(&scope), config, Some("credential:2"), Some("m")),
            0.0
        );

        controller.record_cost_outcome(
            Some(&scope),
            config,
            Some("credential:1"),
            Some("m"),
            0.05,
            0.2,
        );
        assert_eq!(read(&controller), 0.0);

        // 累计额不超过本次下限的 20 倍。
        for _ in 0..100 {
            controller.record_cost_outcome(
                Some(&scope),
                config,
                Some("credential:1"),
                Some("m"),
                0.01,
                0.0,
            );
        }
        assert!((read(&controller) - 0.2).abs() < 1e-12);
    }

    #[test]
    fn cost_deficit_state_is_bounded_per_conversation() {
        let controller = PromptCacheCreationController::default();
        let config = enabled_config();
        let scope = scope();
        // 同一会话反复记账只占一个条目，累计额有上限。
        for _ in 0..10_000 {
            controller.record_cost_outcome(Some(&scope), config, None, Some("m"), 0.05, 0.0);
        }
        assert_eq!(controller.state_count(), 1);
        assert!(controller.cost_deficit_usd(Some(&scope), config, None, Some("m")) <= 1.0 + 1e-9);

        // 没有差额时不新建条目。
        let other = PromptCacheScope::new("conversation-b".to_string(), None);
        controller.record_cost_outcome(Some(&other), config, None, Some("m"), 0.05, 0.2);
        assert_eq!(controller.state_count(), 1);

        // creation control 关闭或没有空闲过期时不记账。
        for config in [
            PromptCacheCreationControlConfig {
                enabled: false,
                ..enabled_config()
            },
            PromptCacheCreationControlConfig {
                expire_after_idle_secs: 0,
                ..enabled_config()
            },
        ] {
            let fresh = PromptCacheCreationController::default();
            fresh.record_cost_outcome(Some(&scope), config, None, Some("m"), 0.05, 0.0);
            assert_eq!(fresh.state_count(), 0);
            assert_eq!(
                fresh.cost_deficit_usd(Some(&scope), config, None, Some("m")),
                0.0
            );
        }
    }

    #[test]
    fn expired_cost_deficit_is_ignored_and_reset() {
        let controller = PromptCacheCreationController::default();
        let config = enabled_config();
        let scope = scope();
        controller.record_cost_outcome(Some(&scope), config, None, Some("m"), 0.05, 0.0);
        let key = CreationControlKey::from_scope(&scope, config.scope_mode, None, Some("m"));
        controller
            .states
            .lock()
            .get_mut(&key)
            .expect("entry")
            .last_seen_at = Some(Utc::now() - chrono::Duration::seconds(3_601));
        assert_eq!(
            controller.cost_deficit_usd(Some(&scope), config, None, Some("m")),
            0.0
        );
        controller.record_cost_outcome(Some(&scope), config, None, Some("m"), 0.05, 0.04);
        assert!(
            (controller.cost_deficit_usd(Some(&scope), config, None, Some("m")) - 0.01).abs()
                < 1e-12
        );
    }

    #[test]
    fn expired_entry_is_reset_instead_of_renewed_by_cost_outcome() {
        let controller = PromptCacheCreationController::default();
        let config = enabled_config();
        let scope = scope();
        let key = CreationControlKey::from_scope(&scope, config.scope_mode, None, Some("m"));
        controller.record_cost_outcome(Some(&scope), config, None, Some("m"), 0.05, 0.0);
        {
            let mut states = controller.states.lock();
            let state = states.get_mut(&key).expect("entry");
            state.last_seen_at = Some(Utc::now() - chrono::Duration::seconds(3_601));
            state.last_creation_at = state.last_seen_at;
            state.successful_requests_since_creation = 7;
        }
        controller.record_cost_outcome(Some(&scope), config, None, Some("m"), 0.05, 0.04);
        let states = controller.states.lock();
        let state = states.get(&key).expect("entry");
        assert!(
            state.last_creation_at.is_none(),
            "stale creation state must be reset"
        );
        assert_eq!(state.successful_requests_since_creation, 0);
        assert!((state.cost_deficit_usd - 0.01).abs() < 1e-12);
    }

    #[test]
    fn small_floor_does_not_truncate_previous_deficit() {
        let controller = PromptCacheCreationController::default();
        let config = enabled_config();
        let scope = scope();
        for _ in 0..20 {
            controller.record_cost_outcome(Some(&scope), config, None, Some("m"), 0.05, 0.0);
        }
        let before = controller.cost_deficit_usd(Some(&scope), config, None, Some("m"));
        assert!((before - 1.0).abs() < 1e-9);
        // 很小的请求不足下限时，不应把此前累计的差额截到 0.001 × 20。
        controller.record_cost_outcome(Some(&scope), config, None, Some("m"), 0.001, 0.0);
        let after = controller.cost_deficit_usd(Some(&scope), config, None, Some("m"));
        assert!((after - before).abs() < 1e-9, "{before} -> {after}");
    }
}
