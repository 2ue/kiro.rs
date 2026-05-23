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

    #[cfg(test)]
    pub fn with_reported_cache_creation_policy(self, policy: ReportedCacheCreationPolicy) -> Self {
        let Some((
            reported_creation,
            cache_creation_5m_input_tokens,
            cache_creation_1h_input_tokens,
        )) = self.reported_cache_creation_fields(policy)
        else {
            return self;
        };

        Self {
            cache_creation_input_tokens: reported_creation,
            cache_creation_5m_input_tokens,
            cache_creation_1h_input_tokens,
            input_tokens: self
                .total_input_tokens
                .saturating_sub(reported_creation)
                .saturating_sub(self.cache_read_input_tokens)
                .max(0),
            ..self
        }
    }

    pub fn with_reported_cache_usage_policy(self, policy: ReportedCacheUsagePolicy) -> Self {
        if !self.has_prompt_cache() {
            return self;
        }

        let mut usage = policy
            .creation_policy()
            .map(|creation_policy| self.with_reported_cache_creation_fields(creation_policy))
            .unwrap_or(self);
        let current_input = self.input_tokens.max(0);
        if current_input <= 0 {
            return usage;
        }

        let reported_input = policy.sample_uncached_input(self).min(current_input).max(1);
        let input_delta = current_input.saturating_sub(reported_input);
        usage.input_tokens = reported_input;
        usage.cache_read_input_tokens = self
            .cache_read_input_tokens
            .max(0)
            .saturating_add(input_delta);
        usage
    }

    fn with_reported_cache_creation_fields(self, policy: ReportedCacheCreationPolicy) -> Self {
        let Some((
            reported_creation,
            cache_creation_5m_input_tokens,
            cache_creation_1h_input_tokens,
        )) = self.reported_cache_creation_fields(policy)
        else {
            return self;
        };

        Self {
            cache_creation_input_tokens: reported_creation,
            cache_creation_5m_input_tokens,
            cache_creation_1h_input_tokens,
            ..self
        }
    }

    fn reported_cache_creation_fields(
        self,
        policy: ReportedCacheCreationPolicy,
    ) -> Option<(i32, i32, i32)> {
        let raw_creation = self.cache_creation_input_tokens.max(0);
        if raw_creation <= 0 {
            return None;
        }

        let cache_read_input_tokens = self.cache_read_input_tokens.max(0);
        let available_creation = self
            .total_input_tokens
            .max(0)
            .saturating_sub(cache_read_input_tokens);
        let effective_max = raw_creation
            .min(policy.normal_max_tokens())
            .min(available_creation);
        if effective_max <= 0 {
            return None;
        }

        let reported_creation = policy.sample(self, effective_max, cache_read_input_tokens > 0);
        let (cache_creation_5m_input_tokens, cache_creation_1h_input_tokens) =
            cap_cache_creation_breakdown(
                self.cache_creation_5m_input_tokens,
                self.cache_creation_1h_input_tokens,
                reported_creation,
            );

        Some((
            reported_creation,
            cache_creation_5m_input_tokens,
            cache_creation_1h_input_tokens,
        ))
    }

    fn has_prompt_cache(self) -> bool {
        self.cache_creation_input_tokens > 0 || self.cache_read_input_tokens > 0
    }

    pub fn billable_input_tokens(self) -> i32 {
        self.input_tokens
            .saturating_add(self.cache_creation_input_tokens)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReportedCacheCreationPolicy {
    target_tokens: i32,
    seed: u64,
}

impl ReportedCacheCreationPolicy {
    #[cfg(test)]
    fn new(target_tokens: i32, seed: u64) -> Option<Self> {
        let target_tokens = target_tokens.max(0);
        (target_tokens > 0).then_some(Self {
            target_tokens,
            seed,
        })
    }

    fn normal_max_tokens(self) -> i32 {
        ((self.target_tokens as i64) * 11 / 10).min(i32::MAX as i64) as i32
    }

    fn sample(self, usage: CacheUsage, effective_max: i32, has_cache_read: bool) -> i32 {
        if effective_max <= 0 {
            return 0;
        }

        let random = self.random_for_usage(usage);
        let bucket_roll = (random % 100) as i32;
        let value_roll = splitmix64(random ^ 0x9e37_79b9_7f4a_7c15);
        if has_cache_read && bucket_roll < 20 {
            return 0;
        }

        let normal_max = self.normal_max_tokens().max(1);
        let buckets: &[(i32, i32, i32)] = if has_cache_read {
            &[(45, 1, 10), (75, 11, 45), (93, 46, 85), (100, 86, 100)]
        } else {
            &[(35, 1, 12), (70, 13, 50), (92, 51, 88), (100, 89, 100)]
        };

        let (_, low_pct, high_pct) = buckets
            .iter()
            .find(|(threshold, _, _)| bucket_roll < *threshold)
            .copied()
            .unwrap_or_else(|| *buckets.last().expect("reported cache buckets"));
        let low = percent_of(normal_max, low_pct).max(1);
        let high = percent_of(normal_max, high_pct).max(low);
        sample_in_range(value_roll, low, high, effective_max)
    }

    fn random_for_usage(self, usage: CacheUsage) -> u64 {
        let mut state = self.seed ^ 0xd6e8_feb8_6659_fd93;
        for value in [
            self.target_tokens,
            usage.total_input_tokens,
            usage.input_tokens,
            usage.cache_creation_input_tokens,
            usage.cache_read_input_tokens,
            usage.cache_creation_5m_input_tokens,
            usage.cache_creation_1h_input_tokens,
        ] {
            state = splitmix64(state ^ value.max(0) as u64);
        }
        state
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReportedCacheUsagePolicy {
    creation_target_tokens: Option<i32>,
    uncached_input_max_tokens: i32,
    seed: u64,
}

impl ReportedCacheUsagePolicy {
    pub fn new(
        creation_target_tokens: i32,
        uncached_input_max_tokens: i32,
        seed: u64,
    ) -> Option<Self> {
        let creation_target_tokens = creation_target_tokens.max(0);
        let uncached_input_max_tokens = uncached_input_max_tokens.max(0);
        (creation_target_tokens > 0 && uncached_input_max_tokens > 0).then_some(Self {
            creation_target_tokens: Some(creation_target_tokens),
            uncached_input_max_tokens,
            seed,
        })
    }

    pub fn input_only(uncached_input_max_tokens: i32, seed: u64) -> Option<Self> {
        let uncached_input_max_tokens = uncached_input_max_tokens.max(0);
        (uncached_input_max_tokens > 0).then_some(Self {
            creation_target_tokens: None,
            uncached_input_max_tokens,
            seed,
        })
    }

    fn creation_policy(self) -> Option<ReportedCacheCreationPolicy> {
        self.creation_target_tokens
            .map(|target_tokens| ReportedCacheCreationPolicy {
                target_tokens,
                seed: self.seed,
            })
    }

    fn sample_uncached_input(self, usage: CacheUsage) -> i32 {
        let max_tokens = self.uncached_input_max_tokens.max(1);
        let random = self.random_for_usage(usage);
        let bucket_roll = (random % 100) as i32;
        let value_roll = splitmix64(random ^ 0xa24b_aed4_963e_e407);

        let buckets: &[(i32, i32, i32)] = &[(70, 2, 25), (95, 26, 70), (100, 71, 100)];
        let (_, low_pct, high_pct) = buckets
            .iter()
            .find(|(threshold, _, _)| bucket_roll < *threshold)
            .copied()
            .unwrap_or_else(|| *buckets.last().expect("reported input buckets"));
        let low = percent_of(max_tokens, low_pct).max(1);
        let high = percent_of(max_tokens, high_pct).max(low);

        sample_in_range(value_roll, low, high, max_tokens)
    }

    fn random_for_usage(self, usage: CacheUsage) -> u64 {
        let mut state = self.seed ^ 0x69b2_3f0a_9c7d_f1e5;
        for value in [
            self.creation_target_tokens.unwrap_or(0),
            self.uncached_input_max_tokens,
            usage.total_input_tokens,
            usage.input_tokens,
            usage.cache_creation_input_tokens,
            usage.cache_read_input_tokens,
            usage.cache_creation_5m_input_tokens,
            usage.cache_creation_1h_input_tokens,
        ] {
            state = splitmix64(state ^ value.max(0) as u64);
        }
        state
    }
}

fn percent_of(value: i32, percent: i32) -> i32 {
    ((value as i64) * (percent as i64) / 100).min(i32::MAX as i64) as i32
}

fn sample_in_range(random: u64, low: i32, high: i32, effective_max: i32) -> i32 {
    let effective_max = effective_max.max(1);
    let mut low = low.clamp(1, effective_max);
    let mut high = high.clamp(1, effective_max);
    if low > high {
        low = 1;
        high = effective_max;
    }

    let span = (high - low + 1) as u64;
    low + (random % span) as i32
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn cap_cache_creation_breakdown(cache5m: i32, cache1h: i32, limit: i32) -> (i32, i32) {
    let limit = limit.max(0);
    let cache5m = cache5m.max(0);
    let cache1h = cache1h.max(0);
    let total = cache5m.saturating_add(cache1h);
    if limit <= 0 || total <= 0 {
        return (0, 0);
    }
    if total <= limit {
        return (cache5m, cache1h);
    }

    let capped_5m = ((cache5m as i64) * (limit as i64) / (total as i64)) as i32;
    let capped_1h = limit.saturating_sub(capped_5m);
    (capped_5m, capped_1h)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CacheAmplification {
    pub token_scale: f64,
    pub max_simulated_input_tokens: i32,
    pub cap_jitter_min_tokens: i32,
    pub cap_jitter_max_tokens: i32,
    pub scale_min_input_tokens: i32,
    pub jitter_seed: u64,
}

impl CacheAmplification {
    const MAX_TOKEN_SCALE: f64 = 3.0;

    pub fn new(
        token_scale: f64,
        max_simulated_input_tokens: i32,
        cap_jitter_min_tokens: i32,
        cap_jitter_max_tokens: i32,
        scale_min_input_tokens: i32,
        jitter_seed: u64,
    ) -> Self {
        let token_scale = if token_scale.is_finite() && token_scale > 0.0 {
            token_scale.clamp(1.0, Self::MAX_TOKEN_SCALE)
        } else {
            1.0
        };
        let mut cap_jitter_min_tokens = cap_jitter_min_tokens.max(0);
        let mut cap_jitter_max_tokens = cap_jitter_max_tokens.max(0);
        if cap_jitter_min_tokens > cap_jitter_max_tokens {
            std::mem::swap(&mut cap_jitter_min_tokens, &mut cap_jitter_max_tokens);
        }

        Self {
            token_scale,
            max_simulated_input_tokens: max_simulated_input_tokens.max(0),
            cap_jitter_min_tokens,
            cap_jitter_max_tokens,
            scale_min_input_tokens: scale_min_input_tokens.max(0),
            jitter_seed,
        }
    }

    pub fn apply(self, base_total_input_tokens: i32) -> i32 {
        let base_total_input_tokens = base_total_input_tokens.max(0);
        if base_total_input_tokens <= 1 || base_total_input_tokens < self.scale_min_input_tokens {
            return base_total_input_tokens;
        }

        let scaled_total = ((base_total_input_tokens as f64) * self.token_scale).round() as i32;
        let scaled_total = scaled_total.max(base_total_input_tokens);
        if self.max_simulated_input_tokens <= 1 {
            return scaled_total;
        }
        if scaled_total <= self.max_simulated_input_tokens {
            return scaled_total;
        }

        let jitter = self.cap_jitter();
        let soft_cap = self
            .max_simulated_input_tokens
            .saturating_sub(jitter)
            .clamp(1, self.max_simulated_input_tokens);
        scaled_total.min(soft_cap)
    }

    fn cap_jitter(self) -> i32 {
        if self.cap_jitter_max_tokens <= 0 || self.max_simulated_input_tokens <= 1 {
            return 0;
        }

        let cap_relative_max = ((self.max_simulated_input_tokens as f64) * 0.08).round() as i32;
        let max_jitter = self
            .cap_jitter_max_tokens
            .min(cap_relative_max)
            .min(self.max_simulated_input_tokens.saturating_sub(1))
            .max(0);
        if max_jitter <= 0 {
            return 0;
        }

        let min_jitter = self.cap_jitter_min_tokens.min(max_jitter).max(0);
        let range = (max_jitter - min_jitter + 1) as u64;
        min_jitter + (self.jitter_seed % range) as i32
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CacheSimulation {
    pub cache_creation_input_tokens: i32,
    pub cache_read_input_tokens: i32,
    pub cache_creation_5m_input_tokens: i32,
    pub cache_creation_1h_input_tokens: i32,
    pub target_cache_ratio: Option<f64>,
    pub amplification: Option<CacheAmplification>,
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
            amplification: None,
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

    pub fn from_prompt_cache_with_ratio_and_amplification(
        usage: PromptCacheUsage,
        target_cache_ratio: f64,
        amplification: Option<CacheAmplification>,
    ) -> Option<Self> {
        let mut simulation = Self::from_prompt_cache_with_ratio(usage, target_cache_ratio)?;
        simulation.amplification = amplification;
        Some(simulation)
    }

    pub fn to_usage(self, total_input_tokens: i32, output_tokens: i32) -> CacheUsage {
        let total_input_tokens = self
            .amplification
            .map(|amplification| amplification.apply(total_input_tokens))
            .unwrap_or_else(|| total_input_tokens.max(0));
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
    build_usage_with_simulation_policy(
        metadata_usage,
        total_input_tokens,
        output_tokens,
        simulation,
        false,
    )
}

pub fn build_usage_with_simulation_policy(
    metadata_usage: Option<&MetadataTokenUsage>,
    total_input_tokens: i32,
    output_tokens: i32,
    simulation: Option<CacheSimulation>,
    fill_zero_metadata_cache_from_simulation: bool,
) -> CacheUsage {
    if let Some(usage) = metadata_usage {
        if fill_zero_metadata_cache_from_simulation && metadata_cache_is_empty(usage) {
            if let Some(simulation) = simulation {
                let output_tokens = if usage.output_tokens > 0 {
                    usage.output_tokens
                } else {
                    output_tokens
                };
                let total_input_tokens = usage.total_input_tokens().max(total_input_tokens);
                return simulation.to_usage(total_input_tokens, output_tokens);
            }
        }

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

pub fn metadata_cache_is_empty(usage: &MetadataTokenUsage) -> bool {
    usage.cache_read_input_tokens <= 0 && usage.cache_write_input_tokens <= 0
}

pub fn usage_has_cache(usage: &CacheUsage) -> bool {
    usage.cache_read_input_tokens > 0 || usage.cache_creation_input_tokens > 0
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
    fn high_cache_policy_fills_zero_metadata_cache_from_larger_local_total() {
        let metadata = MetadataTokenUsage {
            uncached_input_tokens: 50_000,
            output_tokens: 42,
            total_tokens: 50_042,
            cache_read_input_tokens: 0,
            cache_write_input_tokens: 0,
        };
        let simulation = CacheSimulation::from_prompt_cache_with_ratio(
            PromptCacheUsage {
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 95_000,
                cache_creation_5m_input_tokens: 0,
                cache_creation_1h_input_tokens: 0,
                effective_cache_ratio: None,
            },
            0.95,
        );

        let usage =
            build_usage_with_simulation_policy(Some(&metadata), 100_000, 7, simulation, true);

        assert_eq!(usage.total_input_tokens, 100_000);
        assert_eq!(usage.output_tokens, 42);
        assert_eq!(usage.cache_read_input_tokens, 95_000);
        assert_eq!(usage.cache_creation_input_tokens, 0);
        assert_eq!(usage.input_tokens, 5_000);
    }

    #[test]
    fn high_cache_policy_uses_local_totals_when_metadata_is_all_zero() {
        let metadata = MetadataTokenUsage {
            uncached_input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            cache_read_input_tokens: 0,
            cache_write_input_tokens: 0,
        };
        let simulation = CacheSimulation::from_prompt_cache_with_ratio(
            PromptCacheUsage {
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 95_000,
                cache_creation_5m_input_tokens: 0,
                cache_creation_1h_input_tokens: 0,
                effective_cache_ratio: None,
            },
            0.95,
        );

        let usage =
            build_usage_with_simulation_policy(Some(&metadata), 100_000, 123, simulation, true);

        assert_eq!(usage.total_input_tokens, 100_000);
        assert_eq!(usage.output_tokens, 123);
        assert_eq!(usage.cache_read_input_tokens, 95_000);
        assert_eq!(usage.input_tokens, 5_000);
    }

    #[test]
    fn high_cache_policy_preserves_nonzero_metadata_cache() {
        let metadata = MetadataTokenUsage {
            uncached_input_tokens: 1200,
            output_tokens: 900,
            total_tokens: 207_300,
            cache_read_input_tokens: 180_000,
            cache_write_input_tokens: 24_000,
        };
        let simulation = CacheSimulation::from_prompt_cache_with_ratio(
            PromptCacheUsage {
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 95_000,
                cache_creation_5m_input_tokens: 0,
                cache_creation_1h_input_tokens: 0,
                effective_cache_ratio: None,
            },
            0.95,
        );

        let usage =
            build_usage_with_simulation_policy(Some(&metadata), 250_000, 7, simulation, true);

        assert_eq!(usage.total_input_tokens, 205_200);
        assert_eq!(usage.input_tokens, 1_200);
        assert_eq!(usage.output_tokens, 900);
        assert_eq!(usage.cache_read_input_tokens, 180_000);
        assert_eq!(usage.cache_creation_input_tokens, 24_000);
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

    #[test]
    fn amplification_scales_simulated_total_for_large_high_cache_requests() {
        let amplification = CacheAmplification::new(1.5, 0, 0, 0, 8_000, 0);
        let usage = CacheSimulation::from_prompt_cache_with_ratio_and_amplification(
            PromptCacheUsage {
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 95_000,
                cache_creation_5m_input_tokens: 0,
                cache_creation_1h_input_tokens: 0,
                effective_cache_ratio: None,
            },
            0.95,
            Some(amplification),
        )
        .unwrap()
        .to_usage(100_000, 1);

        assert_eq!(usage.total_input_tokens, 150_000);
        assert_eq!(usage.cache_read_input_tokens, 142_500);
        assert_eq!(usage.input_tokens, 7_500);
    }

    #[test]
    fn amplification_does_not_scale_small_requests() {
        let amplification = CacheAmplification::new(2.0, 200_000, 5_000, 20_000, 8_000, 0);
        let usage = CacheSimulation::from_prompt_cache_with_ratio_and_amplification(
            PromptCacheUsage {
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 95_000,
                cache_creation_5m_input_tokens: 0,
                cache_creation_1h_input_tokens: 0,
                effective_cache_ratio: None,
            },
            0.95,
            Some(amplification),
        )
        .unwrap()
        .to_usage(4_096, 1);

        assert_eq!(usage.total_input_tokens, 4_096);
        assert_eq!(usage.cache_read_input_tokens, 3_891);
        assert_eq!(usage.input_tokens, 205);
    }

    #[test]
    fn amplification_uses_deterministic_soft_cap_instead_of_fixed_cap() {
        let first = CacheAmplification::new(3.0, 200_000, 5_000, 20_000, 8_000, 0);
        let second = CacheAmplification::new(3.0, 200_000, 5_000, 20_000, 8_000, 10_000);

        let first_total = first.apply(100_000);
        let second_total = second.apply(100_000);

        assert!((184_000..=195_000).contains(&first_total));
        assert!((184_000..=195_000).contains(&second_total));
        assert_ne!(first_total, 200_000);
        assert_ne!(second_total, 200_000);
        assert!(
            (first_total - second_total).abs() >= 5_000,
            "soft cap jitter should be visible at the k-token level"
        );
    }

    #[test]
    fn invalid_amplification_config_is_sanitized() {
        let amplification = CacheAmplification::new(-2.0, 0, 20_000, 5_000, -1, 0);

        assert_eq!(amplification.apply(10_000), 10_000);
        assert_eq!(amplification.cap_jitter_min_tokens, 5_000);
        assert_eq!(amplification.cap_jitter_max_tokens, 20_000);
    }

    #[test]
    fn reported_creation_policy_rewrites_only_writer_and_preserves_read() {
        let usage = CacheUsage {
            total_input_tokens: 120_000,
            input_tokens: 5_000,
            output_tokens: 9,
            cache_creation_input_tokens: 40_000,
            cache_read_input_tokens: 75_000,
            cache_creation_5m_input_tokens: 30_000,
            cache_creation_1h_input_tokens: 10_000,
        };
        let policy = ReportedCacheCreationPolicy::new(3_000, 13).unwrap();

        let reported = usage.with_reported_cache_creation_policy(policy);

        assert_eq!(reported.total_input_tokens, 120_000);
        assert_eq!(reported.output_tokens, 9);
        assert_eq!(reported.cache_read_input_tokens, 75_000);
        assert!((0..=3_300).contains(&reported.cache_creation_input_tokens));
        assert_eq!(
            reported.cache_creation_input_tokens,
            reported
                .cache_creation_5m_input_tokens
                .saturating_add(reported.cache_creation_1h_input_tokens)
        );
        assert_eq!(
            reported.input_tokens,
            reported
                .total_input_tokens
                .saturating_sub(reported.cache_read_input_tokens)
                .saturating_sub(reported.cache_creation_input_tokens)
        );
    }

    #[test]
    fn reported_creation_policy_can_report_zero_writer_when_read_exists() {
        let usage = CacheUsage {
            total_input_tokens: 120_000,
            input_tokens: 5_000,
            output_tokens: 9,
            cache_creation_input_tokens: 40_000,
            cache_read_input_tokens: 75_000,
            cache_creation_5m_input_tokens: 40_000,
            cache_creation_1h_input_tokens: 0,
        };

        let found_zero = (0..500).any(|seed| {
            usage
                .with_reported_cache_creation_policy(
                    ReportedCacheCreationPolicy::new(3_000, seed).unwrap(),
                )
                .cache_creation_input_tokens
                == 0
        });

        assert!(found_zero);
    }

    #[test]
    fn reported_creation_policy_keeps_writer_nonzero_without_read() {
        let usage = CacheUsage {
            total_input_tokens: 120_000,
            input_tokens: 80_000,
            output_tokens: 9,
            cache_creation_input_tokens: 40_000,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 30_000,
            cache_creation_1h_input_tokens: 10_000,
        };

        for seed in 0..200 {
            let reported = usage.with_reported_cache_creation_policy(
                ReportedCacheCreationPolicy::new(3_000, seed).unwrap(),
            );
            assert!(
                (1..=3_300).contains(&reported.cache_creation_input_tokens),
                "seed {seed} produced {}",
                reported.cache_creation_input_tokens
            );
        }
    }

    #[test]
    fn reported_creation_policy_samples_natural_non_monotonic_values() {
        let usage = CacheUsage {
            total_input_tokens: 120_000,
            input_tokens: 80_000,
            output_tokens: 9,
            cache_creation_input_tokens: 40_000,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 40_000,
            cache_creation_1h_input_tokens: 0,
        };
        let values: Vec<i32> = (0..24)
            .map(|seed| {
                usage
                    .with_reported_cache_creation_policy(
                        ReportedCacheCreationPolicy::new(3_000, seed).unwrap(),
                    )
                    .cache_creation_input_tokens
            })
            .collect();

        assert!(values.iter().all(|value| (1..=3_300).contains(value)));
        assert!(values.windows(2).any(|pair| pair[1] < pair[0]));
        assert!(values.iter().any(|value| value % 10 != 0));
    }

    #[test]
    fn reported_usage_policy_moves_uncached_input_into_read_cache() {
        let usage = CacheUsage {
            total_input_tokens: 120_000,
            input_tokens: 5_000,
            output_tokens: 9,
            cache_creation_input_tokens: 40_000,
            cache_read_input_tokens: 75_000,
            cache_creation_5m_input_tokens: 30_000,
            cache_creation_1h_input_tokens: 10_000,
        };
        let policy = ReportedCacheUsagePolicy::new(3_000, 96, 13).unwrap();

        let reported = usage.with_reported_cache_usage_policy(policy);

        assert_eq!(reported.total_input_tokens, 120_000);
        assert_eq!(reported.output_tokens, 9);
        assert!((0..=3_300).contains(&reported.cache_creation_input_tokens));
        assert!((1..=96).contains(&reported.input_tokens));
        assert_eq!(
            reported.cache_read_input_tokens,
            usage
                .cache_read_input_tokens
                .saturating_add(usage.input_tokens.saturating_sub(reported.input_tokens))
        );
        assert!(reported.cache_read_input_tokens < 80_000);
    }

    #[test]
    fn reported_usage_policy_only_moves_uncached_input_delta_into_read_cache() {
        let usage = CacheUsage {
            total_input_tokens: 120_000,
            input_tokens: 80_000,
            output_tokens: 9,
            cache_creation_input_tokens: 40_000,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 30_000,
            cache_creation_1h_input_tokens: 10_000,
        };
        let policy = ReportedCacheUsagePolicy::new(3_000, 96, 29).unwrap();

        let reported = usage.with_reported_cache_usage_policy(policy);

        assert!((1..=3_300).contains(&reported.cache_creation_input_tokens));
        assert!((1..=96).contains(&reported.input_tokens));
        assert_eq!(
            reported.cache_read_input_tokens,
            usage.input_tokens.saturating_sub(reported.input_tokens)
        );
        assert!(reported.cache_read_input_tokens < 80_000);
    }

    #[test]
    fn input_only_reported_usage_policy_preserves_writer() {
        let usage = CacheUsage {
            total_input_tokens: 120_000,
            input_tokens: 80_000,
            output_tokens: 9,
            cache_creation_input_tokens: 40_000,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 30_000,
            cache_creation_1h_input_tokens: 10_000,
        };
        let policy = ReportedCacheUsagePolicy::input_only(96, 29).unwrap();

        let reported = usage.with_reported_cache_usage_policy(policy);

        assert_eq!(reported.cache_creation_input_tokens, 40_000);
        assert_eq!(reported.cache_creation_5m_input_tokens, 30_000);
        assert_eq!(reported.cache_creation_1h_input_tokens, 10_000);
        assert!((1..=96).contains(&reported.input_tokens));
        assert_eq!(
            reported.cache_read_input_tokens,
            usage.input_tokens.saturating_sub(reported.input_tokens)
        );
    }

    #[test]
    fn reported_usage_policy_does_not_fabricate_cache_for_uncached_usage() {
        let usage = CacheUsage {
            total_input_tokens: 120_000,
            input_tokens: 120_000,
            output_tokens: 9,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        };
        let policy = ReportedCacheUsagePolicy::new(3_000, 96, 29).unwrap();

        let reported = usage.with_reported_cache_usage_policy(policy);

        assert_eq!(reported, usage);
    }

    #[test]
    fn reported_usage_policy_samples_uncached_input_naturally() {
        let usage = CacheUsage {
            total_input_tokens: 120_000,
            input_tokens: 80_000,
            output_tokens: 9,
            cache_creation_input_tokens: 40_000,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 40_000,
            cache_creation_1h_input_tokens: 0,
        };
        let values: Vec<i32> = (0..32)
            .map(|seed| {
                usage
                    .with_reported_cache_usage_policy(
                        ReportedCacheUsagePolicy::new(3_000, 96, seed).unwrap(),
                    )
                    .input_tokens
            })
            .collect();

        assert!(values.iter().all(|value| (1..=96).contains(value)));
        assert!(values.windows(2).any(|pair| pair[1] < pair[0]));
        assert!(values.iter().any(|value| value % 10 != 0));
        assert!(values.iter().any(|value| *value > 25));
    }
}
