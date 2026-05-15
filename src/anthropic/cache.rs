use serde_json::json;

use crate::anthropic::{prompt_cache::PromptCacheUsage, types::Message};
use crate::kiro::model::events::MetadataTokenUsage;
use crate::token;

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheSimulation {
    pub cache_creation_input_tokens: i32,
    pub cache_read_input_tokens: i32,
    pub cache_creation_5m_input_tokens: i32,
    pub cache_creation_1h_input_tokens: i32,
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
        };
        (!simulation.is_empty()).then_some(simulation)
    }

    pub fn heuristic(cached_msg_tokens: i32, total_input_tokens: i32) -> Option<Self> {
        let usage = split_simulated_cache_tokens(cached_msg_tokens, total_input_tokens, 0);
        let simulation = Self {
            cache_creation_input_tokens: usage.cache_creation_input_tokens,
            cache_read_input_tokens: usage.cache_read_input_tokens,
            cache_creation_5m_input_tokens: usage.cache_creation_5m_input_tokens,
            cache_creation_1h_input_tokens: usage.cache_creation_1h_input_tokens,
        };
        (!simulation.is_empty()).then_some(simulation)
    }

    pub fn forced_high_cache(total_input_tokens: i32, high_cache_threshold: i32) -> Option<Self> {
        if total_input_tokens <= 1 {
            return None;
        }
        let target_read = ((total_input_tokens as f64) * 0.9).round() as i32;
        let read = target_read
            .max(high_cache_threshold.max(0))
            .min(total_input_tokens.saturating_sub(1));
        if read <= 0 {
            return None;
        }
        Some(Self {
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: read,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        })
    }

    pub fn to_usage(self, total_input_tokens: i32, output_tokens: i32) -> CacheUsage {
        let total_input_tokens = total_input_tokens.max(0);
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
}

/// Returns an estimated token count for the last message content array that
/// carries a non-null `cache_control` block.
///
/// This mirrors the lightweight cache simulation used by the reference Rust
/// implementation. It intentionally avoids mutating the prompt and only affects
/// Anthropic-compatible usage fields when Kiro does not provide authoritative
/// metadata usage.
pub fn estimate_cached_message_tokens(messages: &[Message]) -> i32 {
    for msg in messages.iter().rev() {
        if let serde_json::Value::Array(blocks) = &msg.content {
            let has_cache_control = blocks
                .iter()
                .any(|block| block.get("cache_control").is_some_and(|v| !v.is_null()));

            if has_cache_control {
                let json_str = serde_json::to_string(blocks).unwrap_or_default();
                return token::count_tokens(&json_str) as i32;
            }
        }
    }

    0
}

fn split_simulated_cache_tokens(
    cached_msg_tokens: i32,
    total_input_tokens: i32,
    output_tokens: i32,
) -> CacheUsage {
    if cached_msg_tokens <= 0 || total_input_tokens <= 0 {
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

    let remaining = total_input_tokens - cached_msg_tokens;
    if remaining <= 0 {
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

    let n_linear = 1.2961e-06 * remaining as f64 + 0.0083;
    let n = if n_linear > 0.03 {
        let hash = ((remaining as u64).wrapping_mul(2_654_435_761)) % 1000;
        0.02 + (hash as f64 / 1000.0) * 0.01
    } else {
        n_linear
    };
    let cache_creation_input_tokens = (remaining as f64 * n).round() as i32;
    let cache_read_input_tokens = remaining - cache_creation_input_tokens;

    CacheUsage {
        total_input_tokens,
        input_tokens: cached_msg_tokens,
        output_tokens,
        cache_creation_input_tokens,
        cache_read_input_tokens,
        cache_creation_5m_input_tokens: cache_creation_input_tokens,
        cache_creation_1h_input_tokens: 0,
    }
}

/// Builds Anthropic-compatible usage fields.
///
/// Authoritative Kiro metadata wins. When metadata is missing, requests with
/// `cache_control` get a simulated prompt-cache split so clients that expect
/// Anthropic cache usage fields see a stable high-cache shape.
#[cfg(test)]
pub fn build_usage(
    metadata_usage: Option<&MetadataTokenUsage>,
    total_input_tokens: i32,
    output_tokens: i32,
    cached_msg_tokens: i32,
) -> CacheUsage {
    build_usage_with_simulation(
        metadata_usage,
        total_input_tokens,
        output_tokens,
        CacheSimulation::heuristic(cached_msg_tokens, total_input_tokens),
    )
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
    use serde_json::json;

    fn make_message(content: serde_json::Value) -> Message {
        Message {
            role: "user".to_string(),
            content,
        }
    }

    #[test]
    fn detects_last_message_with_cache_control() {
        let messages = vec![
            make_message(json!("plain text")),
            make_message(json!([
                {
                    "type": "text",
                    "text": "cached prefix",
                    "cache_control": {"type": "ephemeral"}
                }
            ])),
            make_message(json!("latest plain text")),
        ];

        assert!(estimate_cached_message_tokens(&messages) > 0);
    }

    #[test]
    fn ignores_null_cache_control() {
        let messages = vec![make_message(json!([
            {"type": "text", "text": "not cached", "cache_control": null}
        ]))];

        assert_eq!(estimate_cached_message_tokens(&messages), 0);
    }

    #[test]
    fn simulated_usage_sums_to_total_input_tokens() {
        let usage = build_usage(None, 200_000, 123, 1_000);

        assert_eq!(
            usage.input_tokens + usage.cache_creation_input_tokens + usage.cache_read_input_tokens,
            usage.total_input_tokens
        );
        assert_eq!(usage.output_tokens, 123);
        assert!(usage.cache_read_input_tokens > usage.cache_creation_input_tokens);
    }

    #[test]
    fn metadata_usage_takes_precedence_over_simulated_cache() {
        let metadata = MetadataTokenUsage {
            uncached_input_tokens: 1200,
            output_tokens: 900,
            total_tokens: 207_300,
            cache_read_input_tokens: 180_000,
            cache_write_input_tokens: 24_000,
        };

        let usage = build_usage(Some(&metadata), 10_000, 5, 500);

        assert_eq!(usage.total_input_tokens, 205_200);
        assert_eq!(usage.input_tokens, 1_200);
        assert_eq!(usage.output_tokens, 900);
        assert_eq!(usage.cache_read_input_tokens, 180_000);
        assert_eq!(usage.cache_creation_input_tokens, 24_000);
        assert_eq!(usage.billable_input_tokens(), 25_200);
    }
}
