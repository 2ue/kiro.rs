use parking_lot::RwLock;

use crate::app_config::AppConfigService;
use crate::model::config::{Config, PromptCacheSimulationMode};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PromptCacheRuntimeConfigSnapshot {
    pub prompt_cache_simulation_mode: PromptCacheSimulationMode,
    pub prompt_cache_target_read_ratio: f64,
    pub prompt_cache_token_scale: f64,
    pub prompt_cache_max_simulated_input_tokens: i32,
    pub prompt_cache_cap_jitter_min_tokens: i32,
    pub prompt_cache_cap_jitter_max_tokens: i32,
    pub prompt_cache_scale_min_input_tokens: i32,
    pub high_cache_threshold: i32,
}

impl PromptCacheRuntimeConfigSnapshot {
    pub fn from_config(config: &Config) -> Self {
        Self {
            prompt_cache_simulation_mode: config.prompt_cache_simulation_mode,
            prompt_cache_target_read_ratio: sanitize_ratio(config.prompt_cache_target_read_ratio),
            prompt_cache_token_scale: sanitize_token_scale(config.prompt_cache_token_scale),
            prompt_cache_max_simulated_input_tokens: config
                .prompt_cache_max_simulated_input_tokens
                .max(0),
            prompt_cache_cap_jitter_min_tokens: config.prompt_cache_cap_jitter_min_tokens.max(0),
            prompt_cache_cap_jitter_max_tokens: config.prompt_cache_cap_jitter_max_tokens.max(0),
            prompt_cache_scale_min_input_tokens: config.prompt_cache_scale_min_input_tokens.max(0),
            high_cache_threshold: config.high_cache_threshold.max(0),
        }
        .normalize_jitter()
    }

    pub fn from_config_and_app_config(config: &Config, app_config: &AppConfigService) -> Self {
        Self::from_config(config).with_app_config(app_config)
    }

    pub fn with_app_config(mut self, app_config: &AppConfigService) -> Self {
        if let Some(value) =
            app_config.get_as::<PromptCacheSimulationMode>("prompt_cache_simulation_mode")
        {
            self.prompt_cache_simulation_mode = value;
        }
        if let Some(value) = app_config.get_as::<f64>("prompt_cache_target_read_ratio") {
            self.prompt_cache_target_read_ratio = sanitize_ratio(value);
        }
        if let Some(value) = app_config.get_as::<f64>("prompt_cache_token_scale") {
            self.prompt_cache_token_scale = sanitize_token_scale(value);
        }
        if let Some(value) = app_config.get_as::<i32>("prompt_cache_max_simulated_input_tokens") {
            self.prompt_cache_max_simulated_input_tokens = value.max(0);
        }
        if let Some(value) = app_config.get_as::<i32>("prompt_cache_cap_jitter_min_tokens") {
            self.prompt_cache_cap_jitter_min_tokens = value.max(0);
        }
        if let Some(value) = app_config.get_as::<i32>("prompt_cache_cap_jitter_max_tokens") {
            self.prompt_cache_cap_jitter_max_tokens = value.max(0);
        }
        if let Some(value) = app_config.get_as::<i32>("prompt_cache_scale_min_input_tokens") {
            self.prompt_cache_scale_min_input_tokens = value.max(0);
        }
        if let Some(value) = app_config.get_as::<i32>("high_cache_threshold") {
            self.high_cache_threshold = value.max(0);
        }
        self.normalize_jitter()
    }

    fn normalize_jitter(mut self) -> Self {
        if self.prompt_cache_cap_jitter_min_tokens > self.prompt_cache_cap_jitter_max_tokens {
            std::mem::swap(
                &mut self.prompt_cache_cap_jitter_min_tokens,
                &mut self.prompt_cache_cap_jitter_max_tokens,
            );
        }
        self
    }
}

pub struct PromptCacheRuntimeConfig {
    inner: RwLock<PromptCacheRuntimeConfigSnapshot>,
}

impl PromptCacheRuntimeConfig {
    pub fn new(snapshot: PromptCacheRuntimeConfigSnapshot) -> Self {
        Self {
            inner: RwLock::new(snapshot),
        }
    }

    pub fn snapshot(&self) -> PromptCacheRuntimeConfigSnapshot {
        *self.inner.read()
    }

    pub fn reload_from_app_config(&self, app_config: &AppConfigService) {
        let next = self.snapshot().with_app_config(app_config);
        *self.inner.write() = next;
    }
}

fn sanitize_ratio(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 0.99)
    } else {
        0.0
    }
}

fn sanitize_token_scale(value: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::config::Config;

    #[test]
    fn snapshot_from_config_sanitizes_bounds() {
        let mut config = Config::default();
        config.prompt_cache_target_read_ratio = 2.0;
        config.prompt_cache_token_scale = f64::NAN;
        config.prompt_cache_max_simulated_input_tokens = -1;
        config.prompt_cache_cap_jitter_min_tokens = 24_000;
        config.prompt_cache_cap_jitter_max_tokens = 12_000;
        config.prompt_cache_scale_min_input_tokens = -10;
        config.high_cache_threshold = -5;

        let snapshot = PromptCacheRuntimeConfigSnapshot::from_config(&config);

        assert_eq!(snapshot.prompt_cache_target_read_ratio, 0.99);
        assert_eq!(snapshot.prompt_cache_token_scale, 1.0);
        assert_eq!(snapshot.prompt_cache_max_simulated_input_tokens, 0);
        assert_eq!(snapshot.prompt_cache_cap_jitter_min_tokens, 12_000);
        assert_eq!(snapshot.prompt_cache_cap_jitter_max_tokens, 24_000);
        assert_eq!(snapshot.prompt_cache_scale_min_input_tokens, 0);
        assert_eq!(snapshot.high_cache_threshold, 0);
    }
}
