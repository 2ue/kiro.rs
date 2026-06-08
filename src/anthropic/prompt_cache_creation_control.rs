use std::collections::{HashMap, VecDeque};

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
    credential_id: Option<u64>,
    conversation_id: String,
    model: String,
}

impl CreationControlKey {
    fn from_scope(scope: &PromptCacheScope, mode: PromptCacheCreationControlScopeMode) -> Self {
        Self {
            credential_id: match mode {
                PromptCacheCreationControlScopeMode::CredentialConversationModel => {
                    Some(scope.credential_id)
                }
                PromptCacheCreationControlScopeMode::ConversationModel => None,
            },
            conversation_id: scope.conversation_id.clone(),
            model: scope.model.clone(),
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
}

#[derive(Debug, Clone, Copy)]
struct CreationWindowEvent {
    at: DateTime<Utc>,
    tokens: i32,
}

impl PromptCacheCreationController {
    pub fn preview_success(
        &self,
        scope: Option<&PromptCacheScope>,
        config: PromptCacheCreationControlConfig,
        usage: CacheUsage,
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
        let key = CreationControlKey::from_scope(scope, config.scope_mode);
        let mut states = self.states.lock();
        prune_idle_states(&mut states, now, config);
        let mut state = states.get(&key).cloned().unwrap_or_default();
        prune_window_events(&mut state, now, config);
        let allowed_creation = allowed_creation_tokens(&state, config, creation, now);
        with_allowed_creation(usage, allowed_creation)
    }

    pub fn apply_success(
        &self,
        scope: Option<&PromptCacheScope>,
        config: PromptCacheCreationControlConfig,
        usage: CacheUsage,
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

        let key = CreationControlKey::from_scope(scope, config.scope_mode);
        if creation <= 0 {
            if let Some(state) = states.get_mut(&key) {
                prune_window_events(state, now, config);
                state.last_seen_at = Some(now);
                state.successful_requests_since_creation =
                    state.successful_requests_since_creation.saturating_add(1);
            }
            return usage;
        }

        let state = states.entry(key).or_default();
        prune_window_events(state, now, config);
        let allowed_creation = allowed_creation_tokens(state, config, creation, now);
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

    pub fn clear_credential(&self, credential_id: u64) {
        self.states.lock().retain(|scope, _| {
            scope
                .credential_id
                .is_none_or(|scope_credential_id| scope_credential_id != credential_id)
        });
    }
}

fn allowed_creation_tokens(
    state: &CreationControlState,
    config: PromptCacheCreationControlConfig,
    creation: i32,
    now: DateTime<Utc>,
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
        allowed = allowed.min(config.max_creation_tokens_per_event);
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
        allowed = allowed.min(remaining);
    }

    allowed.max(0)
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
    states.retain(|_, state| {
        state.last_seen_at.is_none_or(|last_seen_at| {
            now.signed_duration_since(last_seen_at).num_seconds()
                < config.expire_after_idle_secs as i64
        })
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> PromptCacheScope {
        PromptCacheScope {
            credential_id: 7,
            conversation_id: "conversation-a".to_string(),
            model: "claude-sonnet-4.6".to_string(),
        }
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
        let config = PromptCacheCreationControlConfig::default();
        let original = usage(20_000);

        let adjusted = controller.apply_success(Some(&scope()), config, original);

        assert_eq!(adjusted, original);
    }

    #[test]
    fn first_creation_is_allowed_but_immediate_next_creation_is_carried_as_input() {
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
    fn creation_is_capped_per_event() {
        let controller = PromptCacheCreationController::default();
        let config = enabled_config();
        let scope = scope();

        let adjusted = controller.apply_success(Some(&scope), config, usage(80_000));

        assert_eq!(adjusted.cache_creation_input_tokens, 30_000);
        assert_eq!(adjusted.input_tokens, 60_000);
        assert_eq!(adjusted.total_input_tokens, 90_000);
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
        assert_eq!(second.cache_creation_input_tokens, 15_000);
        assert_eq!(second.input_tokens, 25_000);
    }

    #[test]
    fn conversation_model_scope_shares_frequency_state_across_credentials() {
        let controller = PromptCacheCreationController::default();
        let mut config = enabled_config();
        config.scope_mode = PromptCacheCreationControlScopeMode::ConversationModel;
        let first_scope = scope();
        let mut second_scope = scope();
        second_scope.credential_id = 8;

        let first = controller.apply_success(Some(&first_scope), config, usage(20_000));
        let second = controller.apply_success(Some(&second_scope), config, usage(12_000));

        assert_eq!(first.cache_creation_input_tokens, 20_000);
        assert_eq!(second.cache_creation_input_tokens, 0);
        assert_eq!(second.input_tokens, 22_000);
    }

    #[test]
    fn credential_conversation_model_scope_keeps_credentials_independent() {
        let controller = PromptCacheCreationController::default();
        let config = enabled_config();
        let first_scope = scope();
        let mut second_scope = scope();
        second_scope.credential_id = 8;

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
            let key = CreationControlKey::from_scope(&scope, config.scope_mode);
            let mut states = controller.states.lock();
            let state = states.get_mut(&key).expect("state after first creation");
            state.last_seen_at = Some(Utc::now() - chrono::Duration::seconds(2));
        }

        let previewed = controller.preview_success(Some(&scope), config, usage(12_000));
        let applied = controller.apply_success(Some(&scope), config, usage(12_000));

        assert_eq!(previewed.cache_creation_input_tokens, 12_000);
        assert_eq!(applied.cache_creation_input_tokens, 12_000);
    }
}
