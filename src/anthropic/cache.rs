use serde_json::json;

use crate::anthropic::prompt_cache::PromptCacheUsage;
use crate::kiro::model::events::MetadataTokenUsage;

/// Usage split used for Anthropic prompt-cache compatible responses.
///
/// `input_tokens` here means the billable uncached portion reported in Anthropic
/// responses, while `total_input_tokens` keeps the full prompt token count used
/// by Kiro metadata/context accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheUsage {
    pub total_input_tokens: i32,
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub cache_creation_input_tokens: i32,
    pub cache_read_input_tokens: i32,
    pub cache_creation_5m_input_tokens: i32,
    pub cache_creation_1h_input_tokens: i32,
}

impl CacheUsage {
    pub fn to_json(self) -> serde_json::Value {
        json!({
            "input_tokens": self.input_tokens,
            "output_tokens": self.output_tokens,
            "cache_creation_input_tokens": self.cache_creation_input_tokens,
            "cache_read_input_tokens": self.cache_read_input_tokens,
            "cache_creation_5m_input_tokens": self.cache_creation_5m_input_tokens,
            "cache_creation_1h_input_tokens": self.cache_creation_1h_input_tokens
        })
    }

    pub fn billable_input_tokens(self) -> i32 {
        self.input_tokens
            .saturating_add(self.cache_creation_input_tokens)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CacheSimulation {
    pub cache_creation_input_tokens: i32,
    pub cache_read_input_tokens: i32,
    pub cache_creation_5m_input_tokens: i32,
    pub cache_creation_1h_input_tokens: i32,
    pub target_cache_ratio: Option<f64>,
}

impl CacheSimulation {
    pub fn is_empty(self) -> bool {
        self.cache_creation_input_tokens <= 0 && self.cache_read_input_tokens <= 0
    }

    pub fn from_prompt_cache(usage: PromptCacheUsage) -> Option<Self> {
        let simulation = Self {
            cache_creation_input_tokens: usage.cache_creation_input_tokens.max(0),
            cache_read_input_tokens: usage.cache_read_input_tokens.max(0),
            cache_creation_5m_input_tokens: usage.cache_creation_5m_input_tokens.max(0),
            cache_creation_1h_input_tokens: usage.cache_creation_1h_input_tokens.max(0),
            target_cache_ratio: usage.effective_cache_ratio,
        };
        (!simulation.is_empty()).then_some(simulation)
    }

    pub fn from_prompt_cache_with_ratio(
        usage: PromptCacheUsage,
        target_cache_ratio: f64,
    ) -> Option<Self> {
        let mut simulation = Self::from_prompt_cache(usage)?;
        simulation.target_cache_ratio = usage.effective_cache_ratio.or(Some(target_cache_ratio));
        Some(simulation)
    }

    pub fn to_usage(self, total_input_tokens: i32, output_tokens: i32) -> CacheUsage {
        let total_input_tokens = total_input_tokens.max(0);
        if let Some(target_ratio) = self.target_cache_ratio {
            return self.to_target_ratio_usage(total_input_tokens, output_tokens, target_ratio);
        }
        let mut cache_creation_input_tokens = self.cache_creation_input_tokens.max(0);
        let mut cache_read_input_tokens = self.cache_read_input_tokens.max(0);
        let cached_total = cache_creation_input_tokens.saturating_add(cache_read_input_tokens);

        if cached_total > total_input_tokens {
            let mut overflow = cached_total - total_input_tokens;
            let read_reduction = cache_read_input_tokens.min(overflow);
            cache_read_input_tokens -= read_reduction;
            overflow -= read_reduction;
            cache_creation_input_tokens = cache_creation_input_tokens.saturating_sub(overflow);
        }

        let cache_creation_5m_input_tokens = self
            .cache_creation_5m_input_tokens
            .max(0)
            .min(cache_creation_input_tokens);
        let cache_creation_1h_input_tokens = self
            .cache_creation_1h_input_tokens
            .max(0)
            .min(cache_creation_input_tokens.saturating_sub(cache_creation_5m_input_tokens));

        CacheUsage {
            total_input_tokens,
            input_tokens: total_input_tokens
                .saturating_sub(cache_creation_input_tokens)
                .saturating_sub(cache_read_input_tokens),
            output_tokens,
            cache_creation_input_tokens,
            cache_read_input_tokens,
            cache_creation_5m_input_tokens,
            cache_creation_1h_input_tokens,
        }
    }

    fn to_target_ratio_usage(
        self,
        total_input_tokens: i32,
        output_tokens: i32,
        target_ratio: f64,
    ) -> CacheUsage {
        if total_input_tokens <= 1 {
            return CacheUsage {
                total_input_tokens,
                input_tokens: total_input_tokens,
                output_tokens,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
                cache_creation_5m_input_tokens: 0,
                cache_creation_1h_input_tokens: 0,
            };
        }

        let target_cached =
            ((total_input_tokens as f64) * target_ratio.clamp(0.0, 0.99)).round() as i32;
        let target_cached = target_cached.clamp(0, total_input_tokens.saturating_sub(1));
        let has_read = self.cache_read_input_tokens > 0;
        let has_creation = self.cache_creation_input_tokens > 0;

        let (cache_read_input_tokens, cache_creation_input_tokens) = match (has_read, has_creation)
        {
            (true, true) => {
                let read = self.cache_read_input_tokens.max(0).min(target_cached);
                (read, target_cached.saturating_sub(read))
            }
            (true, false) => (target_cached, 0),
            (false, true) => (0, target_cached),
            (false, false) => (0, 0),
        };

        let cache_creation_1h_input_tokens = if self.cache_creation_1h_input_tokens > 0 {
            cache_creation_input_tokens
        } else {
            0
        };
        let cache_creation_5m_input_tokens =
            cache_creation_input_tokens.saturating_sub(cache_creation_1h_input_tokens);

        CacheUsage {
            total_input_tokens,
            input_tokens: total_input_tokens
                .saturating_sub(cache_creation_input_tokens)
                .saturating_sub(cache_read_input_tokens),
            output_tokens,
            cache_creation_input_tokens,
            cache_read_input_tokens,
            cache_creation_5m_input_tokens,
            cache_creation_1h_input_tokens,
        }
    }
}

pub fn build_usage_with_simulation(
    metadata_usage: Option<&MetadataTokenUsage>,
    total_input_tokens: i32,
    output_tokens: i32,
    simulation: Option<CacheSimulation>,
) -> CacheUsage {
    if let Some(usage) = metadata_usage {
        return CacheUsage {
            total_input_tokens: usage.total_input_tokens(),
            input_tokens: usage.input_tokens(),
            output_tokens: usage.output_tokens,
            cache_creation_input_tokens: usage.cache_write_input_tokens,
            cache_read_input_tokens: usage.cache_read_input_tokens,
            cache_creation_5m_input_tokens: usage.cache_write_input_tokens,
            cache_creation_1h_input_tokens: 0,
        };
    }

    simulation
        .map(|usage| usage.to_usage(total_input_tokens, output_tokens))
        .unwrap_or(CacheUsage {
            total_input_tokens,
            input_tokens: total_input_tokens,
            output_tokens,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_usage_takes_precedence_over_simulated_cache() {
        let metadata = MetadataTokenUsage {
            uncached_input_tokens: 1200,
            output_tokens: 900,
            total_tokens: 207_300,
            cache_read_input_tokens: 180_000,
            cache_write_input_tokens: 24_000,
        };

        let usage = build_usage_with_simulation(Some(&metadata), 10_000, 5, None);

        assert_eq!(usage.total_input_tokens, 205_200);
        assert_eq!(usage.input_tokens, 1_200);
        assert_eq!(usage.output_tokens, 900);
        assert_eq!(usage.cache_read_input_tokens, 180_000);
        assert_eq!(usage.cache_creation_input_tokens, 24_000);
        assert_eq!(usage.billable_input_tokens(), 25_200);
    }

    #[test]
    fn target_ratio_simulation_scales_creation_to_final_input_total() {
        let simulation = CacheSimulation::from_prompt_cache_with_ratio(
            PromptCacheUsage {
                cache_creation_input_tokens: 95_000,
                cache_read_input_tokens: 0,
                cache_creation_5m_input_tokens: 95_000,
                cache_creation_1h_input_tokens: 0,
                effective_cache_ratio: None,
            },
            0.95,
        )
        .unwrap();

        let usage = simulation.to_usage(200_000, 10);

        assert_eq!(usage.cache_read_input_tokens, 0);
        assert_eq!(usage.cache_creation_input_tokens, 190_000);
        assert_eq!(usage.cache_creation_5m_input_tokens, 190_000);
        assert_eq!(usage.input_tokens, 10_000);
    }

    #[test]
    fn prompt_cache_effective_ratio_overrides_configured_target() {
        let simulation = CacheSimulation::from_prompt_cache_with_ratio(
            PromptCacheUsage {
                cache_creation_input_tokens: 95_000,
                cache_read_input_tokens: 0,
                cache_creation_5m_input_tokens: 95_000,
                cache_creation_1h_input_tokens: 0,
                effective_cache_ratio: Some(0.9325),
            },
            0.95,
        )
        .unwrap();

        let usage = simulation.to_usage(200_000, 10);

        assert_eq!(usage.cache_creation_input_tokens, 186_500);
        assert_eq!(usage.input_tokens, 13_500);
    }

    #[test]
    fn target_ratio_simulation_only_reports_read_after_tracker_match() {
        let creation_only = CacheSimulation::from_prompt_cache_with_ratio(
            PromptCacheUsage {
                cache_creation_input_tokens: 95_000,
                cache_read_input_tokens: 0,
                cache_creation_5m_input_tokens: 95_000,
                cache_creation_1h_input_tokens: 0,
                effective_cache_ratio: None,
            },
            0.95,
        )
        .unwrap()
        .to_usage(100_000, 1);
        assert_eq!(creation_only.cache_read_input_tokens, 0);
        assert_eq!(creation_only.cache_creation_input_tokens, 95_000);

        let read_match = CacheSimulation::from_prompt_cache_with_ratio(
            PromptCacheUsage {
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 95_000,
                cache_creation_5m_input_tokens: 0,
                cache_creation_1h_input_tokens: 0,
                effective_cache_ratio: None,
            },
            0.95,
        )
        .unwrap()
        .to_usage(100_000, 1);
        assert_eq!(read_match.cache_read_input_tokens, 95_000);
        assert_eq!(read_match.cache_creation_input_tokens, 0);
    }
}
