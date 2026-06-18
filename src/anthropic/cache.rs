use serde_json::json;

use crate::anthropic::prompt_cache::PromptCacheUsage;
use crate::kiro::model::events::MetadataTokenUsage;
use crate::model::config::{
    ReportedUsageFieldMode, ReportedUsageFieldPolicy, ReportedUsagePathPolicy,
};

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
    pub fn to_anthropic_usage_json(self) -> serde_json::Value {
        json!({
            "input_tokens": self.input_tokens,
            "output_tokens": self.output_tokens,
            "cache_creation_input_tokens": self.cache_creation_input_tokens,
            "cache_read_input_tokens": self.cache_read_input_tokens
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

    #[cfg(test)]
    pub fn with_reported_cache_usage_policy(self, policy: ReportedCacheUsagePolicy) -> Self {
        self.with_reported_cache_usage_policy_and_raw(policy, self.raw_projection())
    }

    pub fn with_reported_cache_usage_policy_and_raw(
        self,
        policy: ReportedCacheUsagePolicy,
        raw: RawUsage,
    ) -> Self {
        let raw = raw.normalized();
        if !self.has_prompt_cache() {
            return self;
        }

        if !policy.reports_local_prompt_cache() {
            return Self::from_raw_usage(raw);
        }

        let mut usage = self;

        usage.input_tokens = policy.input_base_value(self.input_tokens, raw.input_tokens);
        usage.output_tokens = policy.field_base_value(
            policy.policy.output.normalized(),
            self.output_tokens,
            raw.output_tokens,
        );
        usage.cache_read_input_tokens = policy.field_base_value(
            policy.policy.cache_read.normalized(),
            self.cache_read_input_tokens,
            raw.cache_read_input_tokens,
        );
        if matches!(
            policy.policy.cache_creation.normalized().mode,
            ReportedUsageFieldMode::Raw
        ) {
            usage.cache_creation_input_tokens = raw.cache_creation_input_tokens;
            usage.cache_creation_5m_input_tokens = raw.cache_creation_5m_input_tokens;
            usage.cache_creation_1h_input_tokens = raw.cache_creation_1h_input_tokens;
        }

        if let Some(output_tokens) = policy.sample_output(usage, usage.output_tokens.max(0)) {
            usage.output_tokens = output_tokens;
        }
        if let Some(cache_read_input_tokens) =
            policy.sample_cache_read(usage, usage.cache_read_input_tokens.max(0))
        {
            usage.cache_read_input_tokens = cache_read_input_tokens;
        }
        if let Some(reported_creation) = policy.sample_cache_creation(usage) {
            let (cache_creation_5m_input_tokens, cache_creation_1h_input_tokens) =
                cap_cache_creation_breakdown(
                    usage.cache_creation_5m_input_tokens,
                    usage.cache_creation_1h_input_tokens,
                    reported_creation,
                );
            usage.cache_creation_input_tokens = reported_creation;
            usage.cache_creation_5m_input_tokens = cache_creation_5m_input_tokens;
            usage.cache_creation_1h_input_tokens = cache_creation_1h_input_tokens;
        }
        let current_input = usage.input_tokens.max(0);
        if current_input > 0 {
            if let Some(reported_input) = policy.sample_input(usage, current_input) {
                let input_delta = current_input.saturating_sub(reported_input);
                usage.input_tokens = reported_input;
                if policy.input_moves_delta_to_cache_read() {
                    usage.cache_read_input_tokens = usage
                        .cache_read_input_tokens
                        .max(0)
                        .saturating_add(input_delta);
                }
            }
        }

        usage = policy.apply_final_cache_read_guard(usage);
        usage.total_input_tokens = usage.reported_total_input_tokens();
        usage
    }

    #[cfg(test)]
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

    #[cfg(test)]
    fn raw_projection(self) -> RawUsage {
        RawUsage {
            input_tokens: self.input_tokens.max(0),
            output_tokens: self.output_tokens.max(0),
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        }
    }

    fn from_raw_usage(raw: RawUsage) -> Self {
        Self {
            total_input_tokens: raw.total_input_tokens(),
            input_tokens: raw.input_tokens,
            output_tokens: raw.output_tokens,
            cache_creation_input_tokens: raw.cache_creation_input_tokens,
            cache_read_input_tokens: raw.cache_read_input_tokens,
            cache_creation_5m_input_tokens: raw.cache_creation_5m_input_tokens,
            cache_creation_1h_input_tokens: raw.cache_creation_1h_input_tokens,
        }
    }

    fn reported_total_input_tokens(self) -> i32 {
        self.input_tokens
            .max(0)
            .saturating_add(self.cache_read_input_tokens.max(0))
            .saturating_add(self.cache_creation_input_tokens.max(0))
    }

    pub fn billable_input_tokens(self) -> i32 {
        self.input_tokens
            .saturating_add(self.cache_creation_input_tokens)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawUsage {
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub cache_creation_input_tokens: i32,
    pub cache_read_input_tokens: i32,
    pub cache_creation_5m_input_tokens: i32,
    pub cache_creation_1h_input_tokens: i32,
}

impl RawUsage {
    pub fn uncached(input_tokens: i32, output_tokens: i32) -> Self {
        Self {
            input_tokens: input_tokens.max(0),
            output_tokens: output_tokens.max(0),
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        }
    }

    fn normalized(self) -> Self {
        Self {
            input_tokens: self.input_tokens.max(0),
            output_tokens: self.output_tokens.max(0),
            cache_creation_input_tokens: self.cache_creation_input_tokens.max(0),
            cache_read_input_tokens: self.cache_read_input_tokens.max(0),
            cache_creation_5m_input_tokens: self
                .cache_creation_5m_input_tokens
                .max(0)
                .min(self.cache_creation_input_tokens.max(0)),
            cache_creation_1h_input_tokens: self.cache_creation_1h_input_tokens.max(0).min(
                self.cache_creation_input_tokens
                    .max(0)
                    .saturating_sub(self.cache_creation_5m_input_tokens.max(0)),
            ),
        }
    }

    fn total_input_tokens(self) -> i32 {
        self.input_tokens
            .max(0)
            .saturating_add(self.cache_read_input_tokens.max(0))
            .saturating_add(self.cache_creation_input_tokens.max(0))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReportedCacheCreationPolicy {
    target_tokens: i32,
    normal_max_multiplier: f64,
    seed: u64,
}

impl ReportedCacheCreationPolicy {
    #[cfg(test)]
    fn new(target_tokens: i32, seed: u64) -> Option<Self> {
        let target_tokens = target_tokens.max(0);
        (target_tokens > 0).then_some(Self {
            target_tokens,
            normal_max_multiplier: 1.1,
            seed,
        })
    }

    fn normal_max_tokens(self) -> i32 {
        ((self.target_tokens as f64) * self.normal_max_multiplier)
            .round()
            .clamp(1.0, i32::MAX as f64) as i32
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

#[derive(Debug, Clone, PartialEq)]
pub struct ReportedCacheUsagePolicy {
    policy: ReportedUsagePathPolicy,
    seed: u64,
}

impl ReportedCacheUsagePolicy {
    pub fn from_path_policy(policy: ReportedUsagePathPolicy, seed: u64) -> Option<Self> {
        Some(Self {
            policy: policy.normalized(),
            seed,
        })
    }

    pub fn should_rewrite_local_prompt_cache_usage(&self, usage: CacheUsage) -> bool {
        if !self.reports_local_prompt_cache() || !usage.has_prompt_cache() {
            return false;
        }

        let input = self.policy.input.normalized();
        let rewrites_input = matches!(input.mode, ReportedUsageFieldMode::SampleMax)
            && input.max_tokens > 0
            && usage.input_tokens > input.max_tokens;
        rewrites_input || self.should_cap_final_cache_read(usage)
    }

    fn reports_local_prompt_cache(&self) -> bool {
        self.policy.enabled
    }

    #[cfg(test)]
    pub fn new(
        creation_target_tokens: i32,
        uncached_input_max_tokens: i32,
        seed: u64,
    ) -> Option<Self> {
        Self::from_path_policy(
            ReportedUsagePathPolicy {
                input: crate::model::config::ReportedUsageFieldPolicy::sample_input_max(
                    uncached_input_max_tokens,
                ),
                cache_creation: crate::model::config::ReportedUsageFieldPolicy::sample_target(
                    creation_target_tokens,
                ),
                ..ReportedUsagePathPolicy::default()
            },
            seed,
        )
    }

    #[cfg(test)]
    pub fn input_only(uncached_input_max_tokens: i32, seed: u64) -> Option<Self> {
        Self::from_path_policy(
            ReportedUsagePathPolicy {
                input: crate::model::config::ReportedUsageFieldPolicy::sample_input_max(
                    uncached_input_max_tokens,
                ),
                ..ReportedUsagePathPolicy::default()
            },
            seed,
        )
    }

    fn input_moves_delta_to_cache_read(&self) -> bool {
        self.policy.input.move_delta_to_cache_read
    }

    fn should_cap_final_cache_read(&self, usage: CacheUsage) -> bool {
        self.final_cache_read_effective_cap(usage)
            .map(|cap| usage.cache_read_input_tokens.max(0) > cap)
            .unwrap_or(false)
    }

    fn field_base_value(
        &self,
        field: ReportedUsageFieldPolicy,
        computed_value: i32,
        raw_value: i32,
    ) -> i32 {
        match field.mode {
            ReportedUsageFieldMode::Raw => raw_value.max(0),
            ReportedUsageFieldMode::Preserve
            | ReportedUsageFieldMode::SampleMax
            | ReportedUsageFieldMode::SampleTarget => computed_value.max(0),
        }
    }

    fn input_base_value(&self, computed_value: i32, raw_value: i32) -> i32 {
        match self.policy.input.normalized().mode {
            ReportedUsageFieldMode::Raw
            | ReportedUsageFieldMode::SampleMax
            | ReportedUsageFieldMode::SampleTarget => raw_value.max(0),
            ReportedUsageFieldMode::Preserve => computed_value.max(0),
        }
    }

    fn sample_cache_creation(&self, usage: CacheUsage) -> Option<i32> {
        let raw_creation = usage.cache_creation_input_tokens.max(0);
        if raw_creation <= 0 {
            return None;
        }
        let field = self.policy.cache_creation.normalized();
        let policy = match field.mode {
            ReportedUsageFieldMode::Raw | ReportedUsageFieldMode::Preserve => return None,
            ReportedUsageFieldMode::SampleMax => ReportedCacheCreationPolicy {
                target_tokens: field.max_tokens,
                normal_max_multiplier: 1.0,
                seed: self.seed,
            },
            ReportedUsageFieldMode::SampleTarget => ReportedCacheCreationPolicy {
                target_tokens: field.target_tokens,
                normal_max_multiplier: field.normal_max_multiplier,
                seed: self.seed,
            },
        };
        if policy.target_tokens <= 0 {
            return None;
        }
        let cache_read_input_tokens = usage.cache_read_input_tokens.max(0);
        let effective_max = raw_creation.min(policy.normal_max_tokens());
        (effective_max > 0)
            .then(|| policy.sample(usage, effective_max, cache_read_input_tokens > 0))
    }

    fn sample_input(&self, usage: CacheUsage, current_input: i32) -> Option<i32> {
        self.sample_field(
            self.policy.input.normalized(),
            usage,
            current_input,
            0xa24b_aed4_963e_e407,
        )
        .map(|value| value.min(current_input).max(1))
    }

    fn sample_output(&self, usage: CacheUsage, current_output: i32) -> Option<i32> {
        self.sample_field(
            self.policy.output.normalized(),
            usage,
            current_output,
            0x6d2b_79f5_aa54_21d1,
        )
    }

    fn sample_cache_read(&self, usage: CacheUsage, current_read: i32) -> Option<i32> {
        self.sample_field(
            self.policy.cache_read.normalized(),
            usage,
            current_read,
            0x94d0_49bb_1331_11eb,
        )
    }

    pub fn apply_final_cache_read_guard(&self, mut usage: CacheUsage) -> CacheUsage {
        if !self.reports_local_prompt_cache() {
            return usage;
        }
        let Some(cap) = self.final_cache_read_effective_cap(usage) else {
            return usage;
        };
        usage.cache_read_input_tokens = usage.cache_read_input_tokens.max(0).min(cap);
        usage.total_input_tokens = usage.reported_total_input_tokens();
        usage
    }

    pub fn apply_final_input_guard(&self, mut usage: CacheUsage) -> CacheUsage {
        if !self.reports_local_prompt_cache() || !usage.has_prompt_cache() {
            return usage;
        }

        let current_input = usage.input_tokens.max(0);
        if current_input <= 0 {
            return usage;
        }
        if !self.should_cap_final_input(current_input) {
            return usage;
        }

        let Some(reported_input) = self.sample_input(usage, current_input) else {
            return usage;
        };
        let input_delta = current_input.saturating_sub(reported_input);
        usage.input_tokens = reported_input;
        if input_delta > 0 && self.input_moves_delta_to_cache_read() {
            usage.cache_read_input_tokens = usage
                .cache_read_input_tokens
                .max(0)
                .saturating_add(input_delta);
        }
        usage.total_input_tokens = usage.reported_total_input_tokens();
        usage
    }

    fn should_cap_final_input(&self, current_input: i32) -> bool {
        if current_input <= 0 {
            return false;
        }
        let field = self.policy.input.normalized();
        match field.mode {
            ReportedUsageFieldMode::SampleMax => {
                let max_tokens = field.max_tokens.max(0);
                max_tokens > 0 && current_input > max_tokens
            }
            ReportedUsageFieldMode::SampleTarget => {
                let policy = ReportedCacheCreationPolicy {
                    target_tokens: field.target_tokens.max(0),
                    normal_max_multiplier: field.normal_max_multiplier,
                    seed: self.seed,
                };
                let max_tokens = policy.normal_max_tokens();
                max_tokens > 0 && current_input > max_tokens
            }
            ReportedUsageFieldMode::Raw | ReportedUsageFieldMode::Preserve => false,
        }
    }

    fn final_cache_read_effective_cap(&self, usage: CacheUsage) -> Option<i32> {
        let max_tokens = self.policy.final_cache_read_max_tokens.max(0);
        if max_tokens <= 0 {
            return None;
        }

        let jitter_min = self
            .policy
            .final_cache_read_jitter_min_tokens
            .max(0)
            .min(max_tokens);
        let jitter_max = self
            .policy
            .final_cache_read_jitter_max_tokens
            .max(0)
            .min(max_tokens);
        let (jitter_min, jitter_max) = if jitter_min <= jitter_max {
            (jitter_min, jitter_max)
        } else {
            (jitter_max, jitter_min)
        };

        let jitter = if jitter_max > 0 {
            sample_zero_based_range(
                splitmix64(self.random_for_usage(usage) ^ 0x4d52_8db9_f7a6_2b3c),
                jitter_min,
                jitter_max,
            )
        } else {
            0
        };
        Some(max_tokens.saturating_sub(jitter))
    }

    fn sample_field(
        &self,
        field: ReportedUsageFieldPolicy,
        usage: CacheUsage,
        current_value: i32,
        salt: u64,
    ) -> Option<i32> {
        if current_value <= 0 {
            return None;
        }
        let max_tokens = match field.mode {
            ReportedUsageFieldMode::Raw | ReportedUsageFieldMode::Preserve => return None,
            ReportedUsageFieldMode::SampleMax => field.max_tokens.max(0).min(current_value),
            ReportedUsageFieldMode::SampleTarget => {
                let policy = ReportedCacheCreationPolicy {
                    target_tokens: field.target_tokens.max(0),
                    normal_max_multiplier: field.normal_max_multiplier,
                    seed: self.seed,
                };
                policy.normal_max_tokens().min(current_value)
            }
        };
        if max_tokens <= 0 {
            return None;
        }

        let random = self.random_for_usage(usage);
        let bucket_roll = (random % 100) as i32;
        let value_roll = splitmix64(random ^ salt);

        let buckets: &[(i32, i32, i32)] = &[(70, 2, 25), (95, 26, 70), (100, 71, 100)];
        let (_, low_pct, high_pct) = buckets
            .iter()
            .find(|(threshold, _, _)| bucket_roll < *threshold)
            .copied()
            .unwrap_or_else(|| *buckets.last().expect("reported input buckets"));
        let low = percent_of(max_tokens, low_pct).max(1);
        let high = percent_of(max_tokens, high_pct).max(low);

        Some(sample_in_range(value_roll, low, high, max_tokens))
    }

    fn random_for_usage(&self, usage: CacheUsage) -> u64 {
        let mut state = self.seed ^ 0x69b2_3f0a_9c7d_f1e5;
        for value in [
            self.policy.input.max_tokens,
            self.policy.input.target_tokens,
            self.policy.output.max_tokens,
            self.policy.output.target_tokens,
            self.policy.cache_read.max_tokens,
            self.policy.cache_read.target_tokens,
            self.policy.cache_creation.max_tokens,
            self.policy.cache_creation.target_tokens,
            self.policy.final_cache_read_max_tokens,
            self.policy.final_cache_read_jitter_min_tokens,
            self.policy.final_cache_read_jitter_max_tokens,
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

fn sample_zero_based_range(random: u64, low: i32, high: i32) -> i32 {
    let low = low.max(0);
    let high = high.max(low);
    let span = (i64::from(high) - i64::from(low) + 1) as u64;
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
        let raw_input_tokens = total_input_tokens.max(0);
        let cache_basis_tokens = self
            .amplification
            .map(|amplification| amplification.apply(raw_input_tokens))
            .unwrap_or(raw_input_tokens);
        if let Some(target_ratio) = self.target_cache_ratio {
            return self.to_target_ratio_usage(
                raw_input_tokens,
                cache_basis_tokens,
                output_tokens,
                target_ratio,
            );
        }
        let mut cache_creation_input_tokens = self.cache_creation_input_tokens.max(0);
        let mut cache_read_input_tokens = self.cache_read_input_tokens.max(0);
        let cached_total = cache_creation_input_tokens.saturating_add(cache_read_input_tokens);

        if cached_total > cache_basis_tokens {
            let mut overflow = cached_total - cache_basis_tokens;
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
            total_input_tokens: raw_input_tokens
                .saturating_add(cache_creation_input_tokens)
                .saturating_add(cache_read_input_tokens),
            input_tokens: raw_input_tokens,
            output_tokens: output_tokens.max(0),
            cache_creation_input_tokens,
            cache_read_input_tokens,
            cache_creation_5m_input_tokens,
            cache_creation_1h_input_tokens,
        }
    }

    fn to_target_ratio_usage(
        self,
        raw_input_tokens: i32,
        cache_basis_tokens: i32,
        output_tokens: i32,
        target_ratio: f64,
    ) -> CacheUsage {
        if raw_input_tokens <= 1 {
            return CacheUsage {
                total_input_tokens: raw_input_tokens,
                input_tokens: raw_input_tokens,
                output_tokens: output_tokens.max(0),
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
                cache_creation_5m_input_tokens: 0,
                cache_creation_1h_input_tokens: 0,
            };
        }

        let target_cached =
            ((cache_basis_tokens.max(0) as f64) * target_ratio.clamp(0.0, 0.99)).round() as i32;
        let target_cached = target_cached.max(0);
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
            total_input_tokens: raw_input_tokens
                .saturating_add(cache_creation_input_tokens)
                .saturating_add(cache_read_input_tokens),
            input_tokens: raw_input_tokens,
            output_tokens: output_tokens.max(0),
            cache_creation_input_tokens,
            cache_read_input_tokens,
            cache_creation_5m_input_tokens,
            cache_creation_1h_input_tokens,
        }
    }
}

#[cfg(test)]
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
                let metadata_input_tokens = if usage.uncached_input_tokens > 0 {
                    usage.uncached_input_tokens
                } else {
                    usage.total_input_tokens()
                };
                let total_input_tokens = metadata_input_tokens.max(total_input_tokens);
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

        assert_eq!(usage.total_input_tokens, 195_000);
        assert_eq!(usage.output_tokens, 42);
        assert_eq!(usage.cache_read_input_tokens, 95_000);
        assert_eq!(usage.cache_creation_input_tokens, 0);
        assert_eq!(usage.input_tokens, 100_000);
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

        assert_eq!(usage.total_input_tokens, 195_000);
        assert_eq!(usage.output_tokens, 123);
        assert_eq!(usage.cache_read_input_tokens, 95_000);
        assert_eq!(usage.input_tokens, 100_000);
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
        assert_eq!(usage.input_tokens, 200_000);
        assert_eq!(usage.total_input_tokens, 390_000);
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
        assert_eq!(usage.input_tokens, 200_000);
        assert_eq!(usage.total_input_tokens, 386_500);
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
        assert_eq!(creation_only.input_tokens, 100_000);
        assert_eq!(creation_only.total_input_tokens, 195_000);

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
        assert_eq!(read_match.input_tokens, 100_000);
        assert_eq!(read_match.total_input_tokens, 195_000);
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

        assert_eq!(usage.total_input_tokens, 242_500);
        assert_eq!(usage.cache_read_input_tokens, 142_500);
        assert_eq!(usage.input_tokens, 100_000);
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

        assert_eq!(usage.total_input_tokens, 7_987);
        assert_eq!(usage.cache_read_input_tokens, 3_891);
        assert_eq!(usage.input_tokens, 4_096);
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

        assert_eq!(
            reported.total_input_tokens,
            reported.reported_total_input_tokens()
        );
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

        assert_eq!(
            reported.total_input_tokens,
            reported.reported_total_input_tokens()
        );
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
    fn reported_usage_policy_samples_input_from_raw_request_value() {
        let usage = CacheUsage {
            total_input_tokens: 195_000,
            input_tokens: 100_000,
            output_tokens: 9,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 95_000,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        };
        let raw = RawUsage::uncached(12_345, 9);
        let policy = ReportedCacheUsagePolicy::input_only(96, 29).unwrap();

        let reported = usage.with_reported_cache_usage_policy_and_raw(policy, raw);

        assert!((1..=96).contains(&reported.input_tokens));
        assert_eq!(
            reported.cache_read_input_tokens,
            usage
                .cache_read_input_tokens
                .saturating_add(raw.input_tokens.saturating_sub(reported.input_tokens))
        );
        assert_eq!(
            reported.total_input_tokens,
            reported.reported_total_input_tokens()
        );
    }

    #[test]
    fn reported_usage_policy_raw_fields_keep_request_input_and_output() {
        let usage = CacheUsage {
            total_input_tokens: 242_500,
            input_tokens: 100_000,
            output_tokens: 777,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 142_500,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        };
        let raw = RawUsage::uncached(100_000, 12);
        let policy =
            ReportedCacheUsagePolicy::from_path_policy(ReportedUsagePathPolicy::default(), 0)
                .unwrap();

        let reported = usage.with_reported_cache_usage_policy_and_raw(policy, raw);

        assert_eq!(reported.input_tokens, 100_000);
        assert_eq!(reported.output_tokens, 12);
        assert_eq!(reported.cache_read_input_tokens, 142_500);
        assert_eq!(reported.cache_creation_input_tokens, 0);
        assert_eq!(reported.total_input_tokens, 242_500);
    }

    #[test]
    fn reported_usage_policy_caps_final_cache_read_after_input_delta() {
        let usage = CacheUsage {
            total_input_tokens: 940_913,
            input_tokens: 662_673,
            output_tokens: 45,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 278_240,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        };
        let raw = RawUsage::uncached(662_673, 45);
        let policy = ReportedCacheUsagePolicy::from_path_policy(
            ReportedUsagePathPolicy {
                final_cache_read_max_tokens: 300_000,
                input: ReportedUsageFieldPolicy::sample_input_max(96),
                ..ReportedUsagePathPolicy::default()
            },
            71,
        )
        .unwrap();

        let reported = usage.with_reported_cache_usage_policy_and_raw(policy, raw);

        assert!((1..=96).contains(&reported.input_tokens));
        assert_eq!(reported.cache_read_input_tokens, 300_000);
        assert_eq!(
            reported.total_input_tokens,
            reported.reported_total_input_tokens()
        );
    }

    #[test]
    fn reported_usage_policy_applies_deterministic_final_cache_read_jitter() {
        let usage = CacheUsage {
            total_input_tokens: 940_913,
            input_tokens: 662_673,
            output_tokens: 45,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 278_240,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        };
        let raw = RawUsage::uncached(662_673, 45);
        let policy = ReportedCacheUsagePolicy::from_path_policy(
            ReportedUsagePathPolicy {
                final_cache_read_max_tokens: 300_000,
                final_cache_read_jitter_min_tokens: 8_000,
                final_cache_read_jitter_max_tokens: 24_000,
                input: ReportedUsageFieldPolicy::sample_input_max(96),
                ..ReportedUsagePathPolicy::default()
            },
            71,
        )
        .unwrap();

        let reported = usage.with_reported_cache_usage_policy_and_raw(policy.clone(), raw);
        let again = usage.with_reported_cache_usage_policy_and_raw(policy, raw);

        assert!((276_000..=292_000).contains(&reported.cache_read_input_tokens));
        assert_eq!(
            reported.cache_read_input_tokens,
            again.cache_read_input_tokens
        );
        assert_eq!(reported.input_tokens, again.input_tokens);
        assert_eq!(
            reported.total_input_tokens,
            reported.reported_total_input_tokens()
        );
    }

    #[test]
    fn reported_usage_policy_final_cache_read_guard_does_not_inflate_small_values() {
        let usage = CacheUsage {
            total_input_tokens: 200_050,
            input_tokens: 50,
            output_tokens: 9,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 200_000,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        };
        let raw = RawUsage::uncached(50, 9);
        let policy = ReportedCacheUsagePolicy::from_path_policy(
            ReportedUsagePathPolicy {
                final_cache_read_max_tokens: 300_000,
                final_cache_read_jitter_min_tokens: 8_000,
                final_cache_read_jitter_max_tokens: 24_000,
                ..ReportedUsagePathPolicy::default()
            },
            71,
        )
        .unwrap();

        let reported = usage.with_reported_cache_usage_policy_and_raw(policy, raw);

        assert_eq!(reported.cache_read_input_tokens, 200_000);
        assert_eq!(reported.total_input_tokens, 200_050);
    }

    #[test]
    fn reported_usage_policy_rewrites_records_when_only_final_cache_read_guard_applies() {
        let usage = CacheUsage {
            total_input_tokens: 1_250_010,
            input_tokens: 10,
            output_tokens: 9,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 1_250_000,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        };
        let policy = ReportedCacheUsagePolicy::from_path_policy(
            ReportedUsagePathPolicy {
                final_cache_read_max_tokens: 1_000_000,
                ..ReportedUsagePathPolicy::default()
            },
            0,
        )
        .unwrap();

        assert!(policy.should_rewrite_local_prompt_cache_usage(usage));
    }

    #[test]
    fn disabled_reported_usage_policy_strips_local_cache_projection() {
        let usage = CacheUsage {
            total_input_tokens: 195_000,
            input_tokens: 100_000,
            output_tokens: 9,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 95_000,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        };
        let raw = RawUsage::uncached(100_000, 9);
        let policy =
            ReportedCacheUsagePolicy::from_path_policy(ReportedUsagePathPolicy::disabled(), 0)
                .unwrap();

        let reported = usage.with_reported_cache_usage_policy_and_raw(policy, raw);

        assert_eq!(reported.total_input_tokens, 100_000);
        assert_eq!(reported.input_tokens, 100_000);
        assert_eq!(reported.cache_read_input_tokens, 0);
        assert_eq!(reported.cache_creation_input_tokens, 0);
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
