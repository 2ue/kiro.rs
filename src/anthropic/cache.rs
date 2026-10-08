use serde_json::json;

use crate::anthropic::pricing::ModelPricing;
use crate::anthropic::prompt_cache::PromptCacheUsage;
use crate::kiro::model::events::MetadataTokenUsage;
use crate::model::config::{
    CacheHitShapingPolicy, ReportedUsageFieldMode, ReportedUsageFieldPolicy,
    ReportedUsagePathPolicy,
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
        self.to_anthropic_usage_json_with_thinking_tokens(None)
    }

    pub fn to_anthropic_usage_json_with_thinking_tokens(
        self,
        thinking_tokens: Option<i32>,
    ) -> serde_json::Value {
        let mut usage = json!({
            "input_tokens": self.input_tokens,
            "output_tokens": self.output_tokens,
            "cache_creation_input_tokens": self.cache_creation_input_tokens,
            "cache_read_input_tokens": self.cache_read_input_tokens
        });

        if let Some(thinking_tokens) = thinking_tokens.filter(|tokens| *tokens > 0) {
            usage["output_tokens_details"] = json!({
                "thinking_tokens": thinking_tokens,
            });
        }

        usage
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
        self.with_reported_cache_usage_policy_and_raw_evidence(policy, raw, false)
    }

    pub(crate) fn with_reported_cache_usage_policy_and_raw_evidence(
        self,
        policy: ReportedCacheUsagePolicy,
        raw: RawUsage,
        cache_read_evidence: bool,
    ) -> Self {
        let raw = raw.normalized();
        if !policy.reports_local_prompt_cache() {
            return Self::from_raw_usage(raw);
        }

        let had_cache_read = cache_read_evidence
            || self.cache_read_input_tokens.max(0) > 0
            || raw.cache_read_input_tokens.max(0) > 0;
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
        usage.output_tokens = policy.apply_output_post_processing(usage, usage.output_tokens);
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
                if input_delta > 0 && policy.input_moves_delta_to_cache_read() {
                    add_input_delta_to_reported_cache(&mut usage, input_delta, had_cache_read);
                }
            }
        }

        usage = policy.apply_final_standard_cache_guards(usage);
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

    pub fn to_cache_usage(self) -> CacheUsage {
        CacheUsage::from_raw_usage(self.normalized())
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
        if !self.reports_local_prompt_cache() {
            return false;
        }

        let input = self.policy.input.normalized();
        let rewrites_input = matches!(input.mode, ReportedUsageFieldMode::SampleMax)
            && input.max_tokens > 0
            && usage.input_tokens > input.max_tokens;
        rewrites_input
            || self.should_rewrite_output(usage)
            || self.should_cap_final_cache_creation(usage)
            || self.should_cap_final_cache_read(usage)
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

    fn should_cap_final_cache_creation(&self, usage: CacheUsage) -> bool {
        self.final_cache_creation_effective_cap(usage)
            .map(|cap| usage.cache_creation_input_tokens.max(0) > cap)
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

    fn should_rewrite_output(&self, usage: CacheUsage) -> bool {
        let output = self.policy.output.normalized();
        let output_tokens = usage.output_tokens.max(0);
        if output_tokens <= 0 {
            return false;
        }
        let rewrites_output_field = match output.mode {
            ReportedUsageFieldMode::SampleMax => output.max_tokens > 0,
            ReportedUsageFieldMode::SampleTarget => output.target_tokens > 0,
            ReportedUsageFieldMode::Raw | ReportedUsageFieldMode::Preserve => false,
        };

        rewrites_output_field
            || self.output_uplift_would_apply(output_tokens)
            || self
                .final_output_effective_cap(usage)
                .is_some_and(|cap| output_tokens > cap)
    }

    fn apply_output_post_processing(&self, usage: CacheUsage, output_tokens: i32) -> i32 {
        if !self.final_output_guard_enabled() {
            return output_tokens.max(0);
        }
        let output_tokens = self.apply_output_uplift(output_tokens);
        self.apply_final_output_guard(usage, output_tokens)
    }

    fn final_output_guard_enabled(&self) -> bool {
        self.policy.final_output_guard_enabled
    }

    fn output_uplift_would_apply(&self, output_tokens: i32) -> bool {
        if !self.final_output_guard_enabled() {
            return false;
        }
        let min_tokens = self.policy.output_uplift_min_tokens.max(0);
        let percent = self.policy.output_uplift_percent.min(200);
        percent > 0 && min_tokens > 0 && output_tokens > min_tokens
    }

    fn apply_output_uplift(&self, output_tokens: i32) -> i32 {
        let output_tokens = output_tokens.max(0);
        if !self.output_uplift_would_apply(output_tokens) {
            return output_tokens;
        }
        uplift_tokens_by_percent(output_tokens, self.policy.output_uplift_percent)
    }

    fn apply_final_output_guard(&self, usage: CacheUsage, output_tokens: i32) -> i32 {
        let output_tokens = output_tokens.max(0);
        let Some(cap) = self.final_output_effective_cap(usage) else {
            return output_tokens;
        };
        output_tokens.min(cap)
    }

    fn sample_cache_read(&self, usage: CacheUsage, current_read: i32) -> Option<i32> {
        self.sample_field(
            self.policy.cache_read.normalized(),
            usage,
            current_read,
            0x94d0_49bb_1331_11eb,
        )
    }

    pub fn apply_final_cache_read_guard(&self, usage: CacheUsage) -> CacheUsage {
        if !self.reports_local_prompt_cache() {
            return usage;
        }
        self.apply_final_cache_read_guard_for_standard_fields(usage)
    }

    fn apply_final_cache_read_guard_for_standard_fields(
        &self,
        mut usage: CacheUsage,
    ) -> CacheUsage {
        let Some(cap) = self.final_cache_read_effective_cap(usage) else {
            return usage;
        };
        usage.cache_read_input_tokens = usage.cache_read_input_tokens.max(0).min(cap);
        usage.total_input_tokens = usage.reported_total_input_tokens();
        usage
    }

    pub fn apply_final_cache_creation_guard(&self, usage: CacheUsage) -> CacheUsage {
        if !self.reports_local_prompt_cache() {
            return usage;
        }
        self.apply_final_cache_creation_guard_for_standard_fields(usage)
    }

    fn apply_final_cache_creation_guard_for_standard_fields(
        &self,
        mut usage: CacheUsage,
    ) -> CacheUsage {
        let Some(cap) = self.final_cache_creation_effective_cap(usage) else {
            return usage;
        };
        usage.cache_creation_input_tokens = usage.cache_creation_input_tokens.max(0).min(cap);
        let (cache_creation_5m_input_tokens, cache_creation_1h_input_tokens) =
            cap_cache_creation_breakdown(
                usage.cache_creation_5m_input_tokens,
                usage.cache_creation_1h_input_tokens,
                usage.cache_creation_input_tokens,
            );
        usage.cache_creation_5m_input_tokens = cache_creation_5m_input_tokens;
        usage.cache_creation_1h_input_tokens = cache_creation_1h_input_tokens;
        usage.total_input_tokens = usage.reported_total_input_tokens();
        usage
    }

    pub fn apply_final_standard_cache_guards(&self, usage: CacheUsage) -> CacheUsage {
        let usage = self.apply_final_cache_read_guard(usage);
        self.apply_final_cache_creation_guard(usage)
    }

    /// Apply only the final downstream-standard cache field caps.
    ///
    /// This intentionally ignores `reportedUsage.enabled`: routes such as `kiro_rs_tool` can opt
    /// out of full reported-usage projection while still keeping public standard fields bounded.
    /// Raw and diagnostic usage snapshots are preserved by their callers.
    pub fn apply_final_standard_cache_guards_for_standard_fields(
        &self,
        usage: CacheUsage,
    ) -> CacheUsage {
        let usage = self.apply_final_cache_read_guard_for_standard_fields(usage);
        self.apply_final_cache_creation_guard_for_standard_fields(usage)
    }

    pub fn apply_final_output_guard_to_usage(&self, mut usage: CacheUsage) -> CacheUsage {
        if !self.reports_local_prompt_cache() {
            return usage;
        }
        if !self.final_output_guard_enabled() {
            return usage;
        }
        usage.output_tokens = self.apply_final_output_guard(usage, usage.output_tokens);
        usage
    }

    pub fn apply_final_input_guard(&self, mut usage: CacheUsage) -> CacheUsage {
        if !self.reports_local_prompt_cache() {
            return usage;
        }

        let had_cache_read = usage.cache_read_input_tokens.max(0) > 0;
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
            add_input_delta_to_reported_cache(&mut usage, input_delta, had_cache_read);
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

    /// 下游 input 的允许上限（sample-max / sample-target 的上限，0 表示不限）。
    pub fn reported_input_max_tokens(&self) -> i32 {
        if !self.reports_local_prompt_cache() {
            return 0;
        }
        let field = self.policy.input.normalized();
        match field.mode {
            ReportedUsageFieldMode::SampleMax => field.max_tokens.max(0),
            ReportedUsageFieldMode::SampleTarget => ReportedCacheCreationPolicy {
                target_tokens: field.target_tokens.max(0),
                normal_max_multiplier: field.normal_max_multiplier,
                seed: self.seed,
            }
            .normal_max_tokens(),
            ReportedUsageFieldMode::Raw | ReportedUsageFieldMode::Preserve => 0,
        }
    }

    /// 下游 cache_read / cache_creation 的最终有效上限（0 表示不限）。
    pub fn final_cache_caps(&self, usage: CacheUsage) -> (i32, i32) {
        if !self.reports_local_prompt_cache() {
            return (0, 0);
        }
        (
            self.final_cache_read_effective_cap(usage).unwrap_or(0),
            self.final_cache_creation_effective_cap(usage).unwrap_or(0),
        )
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

    fn final_cache_creation_effective_cap(&self, usage: CacheUsage) -> Option<i32> {
        let max_tokens = self.policy.final_cache_creation_max_tokens.max(0);
        if max_tokens <= 0 {
            return None;
        }

        let jitter_min = self
            .policy
            .final_cache_creation_jitter_min_tokens
            .max(0)
            .min(max_tokens);
        let jitter_max = self
            .policy
            .final_cache_creation_jitter_max_tokens
            .max(0)
            .min(max_tokens);
        let (jitter_min, jitter_max) = if jitter_min <= jitter_max {
            (jitter_min, jitter_max)
        } else {
            (jitter_max, jitter_min)
        };

        let jitter = if jitter_max > 0 {
            sample_zero_based_range(
                splitmix64(self.random_for_usage(usage) ^ 0x17e2_9f4b_8c05_c631),
                jitter_min,
                jitter_max,
            )
        } else {
            0
        };
        Some(max_tokens.saturating_sub(jitter))
    }

    fn final_output_effective_cap(&self, usage: CacheUsage) -> Option<i32> {
        if !self.final_output_guard_enabled() {
            return None;
        }
        let max_tokens = self.policy.final_output_max_tokens.max(0);
        if max_tokens <= 0 {
            return None;
        }

        let jitter_min = self
            .policy
            .final_output_jitter_min_tokens
            .max(0)
            .min(max_tokens);
        let jitter_max = self
            .policy
            .final_output_jitter_max_tokens
            .max(0)
            .min(max_tokens);
        let (jitter_min, jitter_max) = if jitter_min <= jitter_max {
            (jitter_min, jitter_max)
        } else {
            (jitter_max, jitter_min)
        };

        let jitter = if jitter_max > 0 {
            sample_zero_based_range(
                splitmix64(self.random_for_usage(usage) ^ 0xb31a_1269_53f0_9d41),
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
            self.policy.final_cache_creation_max_tokens,
            self.policy.final_cache_creation_jitter_min_tokens,
            self.policy.final_cache_creation_jitter_max_tokens,
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

fn uplift_tokens_by_percent(tokens: i32, percent: u32) -> i32 {
    let tokens = tokens.max(0);
    let percent = percent.min(200);
    if tokens <= 0 || percent == 0 {
        return tokens;
    }
    let numerator = tokens as i64 * (100 + percent) as i64;
    ((numerator + 99) / 100).clamp(0, i32::MAX as i64) as i32
}

fn add_input_delta_to_reported_cache(
    usage: &mut CacheUsage,
    input_delta: i32,
    had_cache_read: bool,
) {
    let input_delta = input_delta.max(0);
    if input_delta <= 0 {
        return;
    }

    if had_cache_read {
        usage.cache_read_input_tokens = usage
            .cache_read_input_tokens
            .max(0)
            .saturating_add(input_delta);
        return;
    }

    add_cache_creation_delta(usage, input_delta);
}

fn add_cache_creation_delta(usage: &mut CacheUsage, delta: i32) {
    let delta = delta.max(0);
    if delta <= 0 {
        return;
    }

    let existing_creation = usage.cache_creation_input_tokens.max(0);
    let (cache_creation_5m_input_tokens, cache_creation_1h_input_tokens) =
        cap_cache_creation_breakdown(
            usage.cache_creation_5m_input_tokens,
            usage.cache_creation_1h_input_tokens,
            existing_creation,
        );
    usage.cache_creation_input_tokens = existing_creation.saturating_add(delta);
    usage.cache_creation_5m_input_tokens = cache_creation_5m_input_tokens.saturating_add(delta);
    usage.cache_creation_1h_input_tokens = cache_creation_1h_input_tokens;
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

/// 命中率整形的请求级输入。
#[derive(Debug, Clone, Copy)]
pub struct CacheHitShapingInput {
    pub policy: CacheHitShapingPolicy,
    /// 请求带历史或本地前缀命中，说明存在可复用前缀。
    pub has_reusable_prefix: bool,
    /// creation control 是否放行本次写入；不放行时优先用 input 承担非读取部分。
    pub creation_allowed: bool,
    /// 原始（未整形）usage，作为计费下限的基准。
    pub raw: CacheUsage,
    /// 模型单价；None 时跳过计费下限。
    pub pricing: Option<ModelPricing>,
    /// 下游 input 的允许范围（来自 reportedUsage.input）；max 为 0 表示不限。
    pub input_min_tokens: i32,
    pub input_max_tokens: i32,
    /// 下游 cache_read / cache_creation 最终上限（0 表示不限）。
    pub max_cache_read_tokens: i32,
    pub max_cache_creation_tokens: i32,
    pub seed: u64,
}

/// 在最终下游 usage 上执行命中率、计费下限和小值整形。
///
/// 总量 `input + read + creation` 保持不变，在总量内重新分配：
/// 1. 按目标命中率（含浮动）确定读取占比，其余为非读取部分；
///    放行写入时非读取部分优先记为写入（input 保持原值），不放行时记为读取 / input；
/// 2. 计费低于原始计费 × `min_cost_ratio` 时降低读取占比，直到不亏损；
/// 3. 非零但过小的读写抬到最小值。
///
/// 是否写入只由 creation control 决定：未放行的请求最终写入为 0，计费差额全部由 input
/// 补足（此时 input 可以超过 reportedUsage.input 上限）。
/// 优先级：写入频次控制 > 不亏损 > 读写上限 > 命中率。
pub fn shape_cache_hit_usage(usage: CacheUsage, input: CacheHitShapingInput) -> CacheUsage {
    let policy = input.policy.normalized();
    if !policy.enabled {
        return usage;
    }
    let usage = normalize_shaped_cache_usage(usage);
    let total = usage.reported_total_input_tokens();
    if total <= 1 {
        return usage;
    }

    let bounds = ShapingBounds::new(&usage, &input);
    let floor = input
        .pricing
        .filter(|_| policy.min_cost_ratio > 0.0)
        .map(|pricing| pricing.estimate(input.raw) * policy.min_cost_ratio);
    let mut shaped = usage;

    let reusable = input.has_reusable_prefix || usage.cache_read_input_tokens > 0;
    let jitter_unit = (splitmix64(input.seed ^ 0x3c6e_f372_fe94_f82b) % 10_001) as f64 / 10_000.0;
    let target = (policy.target_hit_ratio + policy.ratio_jitter * jitter_unit)
        .min(CacheHitShapingPolicy::MAX_TARGET_HIT_RATIO);
    let shape_ratio = reusable && policy.target_hit_ratio > 0.0;
    if shape_ratio {
        shaped = solve_split(
            &usage,
            total,
            target,
            &bounds,
            policy,
            input.creation_allowed,
        );
        if let (Some(floor), Some(pricing)) = (floor, input.pricing)
            && !meets_floor(pricing, &shaped, floor)
        {
            shaped = lossless_split(
                &usage, total, target, &bounds, policy, &input, pricing, floor,
            )
            .unwrap_or(shaped);
        }
    }

    raise_small_cache_fields(&mut shaped, policy, &bounds, input.creation_allowed);
    if !input.creation_allowed && shaped.cache_creation_input_tokens > 0 {
        // creation control 未放行：本次不上报写入，并回 input。
        shaped.input_tokens = shaped
            .input_tokens
            .saturating_add(shaped.cache_creation_input_tokens);
        shaped.cache_creation_input_tokens = 0;
    }
    if let (Some(floor), Some(pricing)) = (floor, input.pricing) {
        raise_cost_to_floor(
            &mut shaped,
            pricing,
            floor,
            policy,
            &bounds,
            input.creation_allowed,
        );
    }
    // 放行写入时把写入抬到最小比例，然后再补读取保持命中率。
    raise_creation_to_min(&mut shaped, &input, policy, &bounds);
    if shape_ratio {
        // 总量内受计费下限限制达不到目标时，在总量之外追加读取：只增不减，计费不会下降。
        raise_read_to_ratio(&mut shaped, target, bounds.read_cap);
    }
    normalize_shaped_cache_usage(shaped)
}

#[derive(Debug, Clone, Copy)]
struct ShapingBounds {
    input_min: i32,
    input_max: i32,
    read_cap: i32,
    creation_cap: i32,
}

impl ShapingBounds {
    fn new(usage: &CacheUsage, input: &CacheHitShapingInput) -> Self {
        let cap = |value: i32| if value > 0 { value } else { i32::MAX };
        let input_max = cap(input.input_max_tokens);
        let input_min = input.input_min_tokens.max(0).min(input_max);
        // input 已经超过上限时（上游 usage 本身更大），不强行压低。
        let input_max = input_max.max(usage.input_tokens.min(input_min.max(input_max)));
        let policy = input.policy.normalized();
        let mut read_cap = cap(input.max_cache_read_tokens);
        if policy.max_read_multiplier > 0.0 {
            let raw_input = input.raw.reported_total_input_tokens().max(1) as f64;
            let multiplier_cap = (raw_input * policy.max_read_multiplier)
                .round()
                .clamp(1.0, i32::MAX as f64) as i32;
            read_cap = read_cap.min(multiplier_cap);
        }
        Self {
            input_min,
            input_max,
            read_cap,
            creation_cap: cap(input.max_cache_creation_tokens),
        }
    }
}

/// 放行写入时，把写入抬到原始输入 × `min_creation_ratio`。
fn raise_creation_to_min(
    usage: &mut CacheUsage,
    input: &CacheHitShapingInput,
    policy: CacheHitShapingPolicy,
    bounds: &ShapingBounds,
) {
    if !input.creation_allowed || policy.min_creation_ratio <= 0.0 {
        return;
    }
    let raw_input = input.raw.reported_total_input_tokens().max(0) as f64;
    let min_creation = (raw_input * policy.min_creation_ratio)
        .round()
        .clamp(0.0, i32::MAX as f64) as i32;
    let min_creation = min_creation
        .max(policy.min_cache_creation_tokens.max(0))
        .min(bounds.creation_cap);
    if usage.cache_creation_input_tokens < min_creation {
        usage.cache_creation_input_tokens = min_creation;
    }
}

/// 在总量 `total` 内按读取占比 `ratio` 分配。
fn solve_split(
    usage: &CacheUsage,
    total: i32,
    ratio: f64,
    bounds: &ShapingBounds,
    policy: CacheHitShapingPolicy,
    allow_creation: bool,
) -> CacheUsage {
    let ratio = ratio.clamp(0.0, CacheHitShapingPolicy::MAX_TARGET_HIT_RATIO);
    let mut read = ((total as f64) * ratio).round() as i32;
    read = read.clamp(0, total).min(bounds.read_cap);
    let non_read = total - read;

    let min_creation = policy.min_cache_creation_tokens.max(0);
    let (mut input, mut creation) = if allow_creation {
        let input = usage
            .input_tokens
            .clamp(bounds.input_min, bounds.input_max)
            .min(non_read);
        (input, non_read - input)
    } else {
        // 未放行写入：非读取部分记为 input，超出 input 上限的部分并入读取，
        // 读取到上限后仍有剩余时留在 input（允许超过 input 上限），不记写入。
        let input = non_read
            .clamp(bounds.input_min, bounds.input_max)
            .min(non_read);
        let overflow = non_read - input;
        let to_read = overflow.min(bounds.read_cap.saturating_sub(read).max(0));
        (input.saturating_add(overflow - to_read), 0)
    };
    if creation > 0 && creation < min_creation {
        if allow_creation && non_read.saturating_sub(min_creation) >= bounds.input_min {
            creation = min_creation;
            input = non_read - creation;
        } else if non_read <= bounds.input_max {
            input = non_read;
            creation = 0;
        }
    }
    if creation > bounds.creation_cap {
        let excess = creation - bounds.creation_cap;
        creation = bounds.creation_cap;
        input = input.saturating_add(excess);
    }
    let read = total - input - creation;
    CacheUsage {
        input_tokens: input,
        cache_read_input_tokens: read.max(0),
        cache_creation_input_tokens: creation,
        ..*usage
    }
}

/// 计费不足时降低读取占比，找到不亏损的最大占比。
#[allow(clippy::too_many_arguments)]
fn lossless_split(
    usage: &CacheUsage,
    total: i32,
    target: f64,
    bounds: &ShapingBounds,
    policy: CacheHitShapingPolicy,
    input: &CacheHitShapingInput,
    pricing: ModelPricing,
    floor: f64,
) -> Option<CacheUsage> {
    let allow_creation = input.creation_allowed;
    {
        let split = |ratio: f64| solve_split(usage, total, ratio, bounds, policy, allow_creation);
        if !meets_floor(pricing, &split(0.0), floor) {
            return None;
        }
        let (mut low, mut high) = (0.0_f64, target);
        for _ in 0..40 {
            let mid = (low + high) / 2.0;
            if meets_floor(pricing, &split(mid), floor) {
                low = mid;
            } else {
                high = mid;
            }
        }
        // 触及不亏损上限时向下浮动，避免每次都是同一个占比。
        let jitter_unit =
            (splitmix64(input.seed ^ 0x9f1d_2b7c_44e8_a613) % 10_001) as f64 / 10_000.0;
        let ratio = (low - policy.ratio_jitter * jitter_unit).max(0.0);
        let shaped = split(ratio);
        if meets_floor(pricing, &shaped, floor) {
            return Some(shaped);
        }
        Some(split(low))
    }
}

/// 增加 read 使 read / (input + read + creation) >= ratio。
fn raise_read_to_ratio(usage: &mut CacheUsage, ratio: f64, read_cap: i32) {
    let others = usage
        .input_tokens
        .max(0)
        .saturating_add(usage.cache_creation_input_tokens.max(0)) as f64;
    if others <= 0.0 || ratio <= 0.0 {
        return;
    }
    let needed = ((ratio * others) / (1.0 - ratio))
        .ceil()
        .min(read_cap as f64) as i32;
    if usage.cache_read_input_tokens < needed {
        usage.cache_read_input_tokens = needed;
    }
}

fn meets_floor(pricing: ModelPricing, usage: &CacheUsage, floor: f64) -> bool {
    pricing.estimate(*usage) >= floor * (1.0 - 1e-9)
}

/// 总量内已无法不亏损时（例如总量本身过小）补足计费：
/// 放行写入时先增加写入、写入到上限后增加 input；未放行时只增加 input，不受 input 上限约束。
fn raise_cost_to_floor(
    usage: &mut CacheUsage,
    pricing: ModelPricing,
    floor: f64,
    policy: CacheHitShapingPolicy,
    bounds: &ShapingBounds,
    creation_allowed: bool,
) {
    if meets_floor(pricing, usage, floor) {
        return;
    }
    if creation_allowed && pricing.cache_creation_input_token_cost > 0.0 {
        let gap = floor - pricing.estimate(*usage);
        let existing = usage.cache_creation_input_tokens.max(0) as i64;
        let mut tokens = (gap / pricing.cache_creation_input_token_cost).ceil() as i64;
        let min_creation = policy.min_cache_creation_tokens.max(0) as i64;
        if existing + tokens < min_creation {
            tokens = min_creation - existing;
        }
        let room = (bounds.creation_cap as i64 - existing).max(0);
        let add = tokens.min(room).max(0);
        if existing + add >= min_creation || existing > 0 {
            usage.cache_creation_input_tokens = (existing + add).min(i32::MAX as i64) as i32;
        }
    }
    if !meets_floor(pricing, usage, floor) && pricing.input_cost_per_token > 0.0 {
        let gap = floor - pricing.estimate(*usage);
        let tokens = (gap / pricing.input_cost_per_token).ceil() as i32;
        let room = if creation_allowed {
            bounds.input_max.saturating_sub(usage.input_tokens).max(0)
        } else {
            i32::MAX
        };
        usage.input_tokens = usage.input_tokens.saturating_add(tokens.min(room).max(0));
    }
}

/// 非零但低于最小值的读写抬到最小值；未放行写入或写入超过上限时，小写入并回 input。
fn raise_small_cache_fields(
    usage: &mut CacheUsage,
    policy: CacheHitShapingPolicy,
    bounds: &ShapingBounds,
    creation_allowed: bool,
) {
    let min_read = policy.min_cache_read_tokens.max(0);
    let read = usage.cache_read_input_tokens.max(0);
    if read > 0 && read < min_read {
        usage.cache_read_input_tokens = min_read.min(bounds.read_cap);
    }

    let min_creation = policy.min_cache_creation_tokens.max(0);
    let creation = usage.cache_creation_input_tokens.max(0);
    if creation > 0 && creation < min_creation {
        if creation_allowed && min_creation <= bounds.creation_cap {
            usage.cache_creation_input_tokens = min_creation;
        } else {
            usage.input_tokens = usage.input_tokens.saturating_add(creation);
            usage.cache_creation_input_tokens = 0;
        }
    }
}

fn normalize_cache_usage_breakdown(mut usage: CacheUsage) -> CacheUsage {
    usage.input_tokens = usage.input_tokens.max(0);
    usage.cache_read_input_tokens = usage.cache_read_input_tokens.max(0);
    usage.cache_creation_input_tokens = usage.cache_creation_input_tokens.max(0);
    let (cache5m, cache1h) = cap_cache_creation_breakdown(
        usage.cache_creation_5m_input_tokens,
        usage.cache_creation_1h_input_tokens,
        usage.cache_creation_input_tokens,
    );
    usage.cache_creation_5m_input_tokens = cache5m;
    usage.cache_creation_1h_input_tokens = cache1h;
    usage.total_input_tokens = usage.reported_total_input_tokens();
    usage
}

/// 命中率整形专用：在 [`normalize_cache_usage_breakdown`] 基础上，把被抬高的写入按已有
/// TTL 归属补齐（只有 1h 时归入 1h，否则归入 5m），保证 5m + 1h == creation。
fn normalize_shaped_cache_usage(usage: CacheUsage) -> CacheUsage {
    let mut usage = normalize_cache_usage_breakdown(usage);
    let assigned = usage
        .cache_creation_5m_input_tokens
        .saturating_add(usage.cache_creation_1h_input_tokens);
    let remainder = usage
        .cache_creation_input_tokens
        .saturating_sub(assigned)
        .max(0);
    if usage.cache_creation_1h_input_tokens > 0 && usage.cache_creation_5m_input_tokens == 0 {
        usage.cache_creation_1h_input_tokens = usage
            .cache_creation_1h_input_tokens
            .saturating_add(remainder);
    } else {
        usage.cache_creation_5m_input_tokens = usage
            .cache_creation_5m_input_tokens
            .saturating_add(remainder);
    }
    usage
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
    pub split_cached_input: bool,
    pub split_input_min_tokens: i32,
    pub split_input_max_tokens: i32,
    pub split_input_jitter_seed: u64,
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
            split_cached_input: false,
            split_input_min_tokens: 0,
            split_input_max_tokens: 0,
            split_input_jitter_seed: 0,
        };
        (!simulation.is_empty()).then_some(simulation)
    }

    pub fn from_prompt_cache_split_input(usage: PromptCacheUsage) -> Option<Self> {
        let mut simulation = Self::from_prompt_cache(usage)?;
        simulation.split_cached_input = true;
        Some(simulation)
    }

    pub fn from_prompt_cache_split_input_with_reported_input_range(
        usage: PromptCacheUsage,
        min_tokens: i32,
        max_tokens: i32,
        jitter_seed: u64,
    ) -> Option<Self> {
        let mut simulation = Self::from_prompt_cache_split_input(usage)?;
        let min_tokens = min_tokens.max(0);
        let max_tokens = max_tokens.max(0);
        simulation.split_input_min_tokens = if max_tokens > 0 {
            min_tokens.min(max_tokens)
        } else {
            min_tokens
        };
        simulation.split_input_max_tokens = max_tokens;
        simulation.split_input_jitter_seed = jitter_seed;
        Some(simulation)
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
        if self.split_cached_input {
            if let Some(covered_ratio) = self.target_cache_ratio {
                let usage =
                    self.to_split_ratio_usage(raw_input_tokens, output_tokens, covered_ratio);
                return self.apply_split_input_range(usage);
            }
        }
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
        let capped_cached_total =
            cache_creation_input_tokens.saturating_add(cache_read_input_tokens);
        let input_tokens = if self.split_cached_input {
            raw_input_tokens.saturating_sub(capped_cached_total).max(0)
        } else {
            raw_input_tokens
        };
        let total_input_tokens = if self.split_cached_input {
            raw_input_tokens
        } else {
            raw_input_tokens
                .saturating_add(cache_creation_input_tokens)
                .saturating_add(cache_read_input_tokens)
        };

        let usage = CacheUsage {
            total_input_tokens,
            input_tokens,
            output_tokens: output_tokens.max(0),
            cache_creation_input_tokens,
            cache_read_input_tokens,
            cache_creation_5m_input_tokens,
            cache_creation_1h_input_tokens,
        };
        if self.split_cached_input {
            self.apply_split_input_range(usage)
        } else {
            usage
        }
    }

    fn to_split_ratio_usage(
        self,
        raw_input_tokens: i32,
        output_tokens: i32,
        covered_ratio: f64,
    ) -> CacheUsage {
        let total_input_tokens = raw_input_tokens.max(0);
        if total_input_tokens <= 0 {
            return CacheUsage {
                total_input_tokens,
                input_tokens: total_input_tokens,
                output_tokens: output_tokens.max(0),
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
                cache_creation_5m_input_tokens: 0,
                cache_creation_1h_input_tokens: 0,
            };
        }

        let cache_total =
            ((total_input_tokens as f64) * covered_ratio.clamp(0.0, 1.0)).round() as i32;
        let cache_total = cache_total.clamp(0, total_input_tokens);
        let raw_read = self.cache_read_input_tokens.max(0);
        let raw_creation = self.cache_creation_input_tokens.max(0);
        let raw_cached_total = raw_read.saturating_add(raw_creation);
        if raw_cached_total <= 0 || cache_total <= 0 {
            return CacheUsage {
                total_input_tokens,
                input_tokens: total_input_tokens,
                output_tokens: output_tokens.max(0),
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
                cache_creation_5m_input_tokens: 0,
                cache_creation_1h_input_tokens: 0,
            };
        }

        let cache_read_input_tokens =
            (((cache_total as f64) * (raw_read as f64 / raw_cached_total as f64)).round() as i32)
                .clamp(0, cache_total);
        let cache_creation_input_tokens = cache_total.saturating_sub(cache_read_input_tokens);
        let (cache_creation_5m_input_tokens, cache_creation_1h_input_tokens) =
            cap_cache_creation_breakdown(
                self.cache_creation_5m_input_tokens,
                self.cache_creation_1h_input_tokens,
                cache_creation_input_tokens,
            );

        CacheUsage {
            total_input_tokens,
            input_tokens: total_input_tokens.saturating_sub(cache_total),
            output_tokens: output_tokens.max(0),
            cache_creation_input_tokens,
            cache_read_input_tokens,
            cache_creation_5m_input_tokens,
            cache_creation_1h_input_tokens,
        }
    }

    fn apply_split_input_range(self, mut usage: CacheUsage) -> CacheUsage {
        if !self.split_cached_input {
            return usage;
        }

        let total = usage.total_input_tokens.max(0);
        if total <= 0 {
            return usage;
        }

        let min_tokens = self.split_input_min_tokens.max(0).min(total);
        let max_tokens = if self.split_input_max_tokens > 0 {
            self.split_input_max_tokens.max(min_tokens).min(total)
        } else {
            total
        };
        if min_tokens <= 0 && self.split_input_max_tokens <= 0 {
            return usage;
        }

        usage.input_tokens = usage.input_tokens.max(0).min(total);
        usage.cache_read_input_tokens = usage.cache_read_input_tokens.max(0);
        usage.cache_creation_input_tokens = usage.cache_creation_input_tokens.max(0);

        let current_input = usage.input_tokens;
        if current_input >= min_tokens && current_input <= max_tokens {
            return normalize_cache_usage_breakdown(usage);
        }

        let target_input = self.split_input_range_target(usage, min_tokens, max_tokens);
        if target_input > current_input {
            let mut delta = target_input - current_input;
            let creation_reduction = usage.cache_creation_input_tokens.min(delta);
            usage.cache_creation_input_tokens -= creation_reduction;
            delta -= creation_reduction;
            let read_reduction = usage.cache_read_input_tokens.min(delta);
            usage.cache_read_input_tokens -= read_reduction;
            usage.input_tokens = total
                .saturating_sub(usage.cache_creation_input_tokens)
                .saturating_sub(usage.cache_read_input_tokens)
                .max(0);
        } else if target_input < current_input {
            let delta = current_input - target_input;
            usage.input_tokens = target_input;
            usage.cache_creation_input_tokens =
                usage.cache_creation_input_tokens.saturating_add(delta);
        }

        normalize_cache_usage_breakdown(usage)
    }

    fn split_input_range_target(self, usage: CacheUsage, min_tokens: i32, max_tokens: i32) -> i32 {
        let min_tokens = min_tokens.max(0);
        let max_tokens = max_tokens.max(min_tokens);
        if max_tokens <= min_tokens {
            return min_tokens;
        }

        let mut state = self.split_input_jitter_seed;
        for value in [
            usage.total_input_tokens,
            usage.input_tokens,
            usage.cache_creation_input_tokens,
            usage.cache_read_input_tokens,
            usage.cache_creation_5m_input_tokens,
            usage.cache_creation_1h_input_tokens,
        ] {
            state = splitmix64(state ^ value.max(0) as u64);
        }
        let span = (max_tokens - min_tokens + 1) as u64;
        min_tokens + (splitmix64(state ^ 0x6751_d5c4_88a2_c713) % span) as i32
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
                // 前缀增长时：读取取未放大的匹配量，放大后的差额整体记为写入。
                // 这样每次前缀变化都会产生较大的写入；命中率由最终 hitShaping 补读取保证。
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
        let resolved =
            usage_from_metadata_or_estimate(Some(usage), total_input_tokens, output_tokens);
        if fill_zero_metadata_cache_from_simulation && metadata_cache_is_empty(usage) {
            if let Some(simulation) = simulation {
                let total_input_tokens = resolved.input_tokens.max(total_input_tokens.max(0));
                return simulation.to_usage(total_input_tokens, resolved.output_tokens);
            }
        }

        return resolved;
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

pub fn usage_from_metadata_or_estimate(
    metadata_usage: Option<&MetadataTokenUsage>,
    input_tokens: i32,
    output_tokens: i32,
) -> CacheUsage {
    let estimated_total_input_tokens = input_tokens.max(0);
    let output_tokens = output_tokens.max(0);
    let Some(usage) = metadata_usage else {
        return CacheUsage {
            total_input_tokens: estimated_total_input_tokens,
            input_tokens: estimated_total_input_tokens,
            output_tokens,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        };
    };

    let output_tokens = if usage.output_tokens > 0 {
        usage.output_tokens
    } else {
        output_tokens
    };
    let cache_creation_input_tokens = usage.cache_write_input_tokens.max(0);
    let cache_read_input_tokens = usage.cache_read_input_tokens.max(0);
    let cached_input_tokens = cache_creation_input_tokens.saturating_add(cache_read_input_tokens);
    let metadata_total_input_tokens = usage.total_tokens.max(0).saturating_sub(output_tokens);
    let fallback_total_input_tokens = if usage.total_tokens > 0 {
        metadata_total_input_tokens
    } else {
        estimated_total_input_tokens
    };
    let input_tokens = if usage.uncached_input_tokens > 0 {
        usage.uncached_input_tokens
    } else {
        fallback_total_input_tokens.saturating_sub(cached_input_tokens)
    };

    CacheUsage {
        total_input_tokens: input_tokens
            .saturating_add(cache_creation_input_tokens)
            .saturating_add(cache_read_input_tokens),
        input_tokens,
        output_tokens,
        cache_creation_input_tokens,
        cache_read_input_tokens,
        cache_creation_5m_input_tokens: cache_creation_input_tokens,
        cache_creation_1h_input_tokens: 0,
    }
}

pub fn metadata_usage_has_signal(usage: &MetadataTokenUsage) -> bool {
    usage.uncached_input_tokens > 0
        || usage.output_tokens > 0
        || usage.cache_read_input_tokens > 0
        || usage.cache_write_input_tokens > 0
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
    fn all_zero_metadata_without_simulation_falls_back_to_local_estimates() {
        let metadata = MetadataTokenUsage::default();

        let usage = build_usage_with_simulation_policy(Some(&metadata), 4_096, 17, None, false);

        assert_eq!(usage.total_input_tokens, 4_096);
        assert_eq!(usage.input_tokens, 4_096);
        assert_eq!(usage.output_tokens, 17);
        assert_eq!(usage.cache_read_input_tokens, 0);
        assert_eq!(usage.cache_creation_input_tokens, 0);
        assert!(!metadata_usage_has_signal(&metadata));
    }

    #[test]
    fn partial_metadata_preserves_cache_and_falls_back_missing_fields() {
        let metadata = MetadataTokenUsage {
            uncached_input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            cache_read_input_tokens: 1_500,
            cache_write_input_tokens: 250,
        };

        let usage = usage_from_metadata_or_estimate(Some(&metadata), 4_096, 17);

        assert_eq!(usage.total_input_tokens, 4_096);
        assert_eq!(usage.input_tokens, 2_346);
        assert_eq!(usage.output_tokens, 17);
        assert_eq!(usage.cache_read_input_tokens, 1_500);
        assert_eq!(usage.cache_creation_input_tokens, 250);
        assert!(metadata_usage_has_signal(&metadata));
    }

    #[test]
    fn partial_metadata_uses_reported_total_to_derive_uncached_input() {
        let metadata = MetadataTokenUsage {
            uncached_input_tokens: 0,
            output_tokens: 100,
            total_tokens: 2_000,
            cache_read_input_tokens: 1_200,
            cache_write_input_tokens: 300,
        };

        let usage = usage_from_metadata_or_estimate(Some(&metadata), 4_096, 17);

        assert_eq!(usage.total_input_tokens, 1_900);
        assert_eq!(usage.input_tokens, 400);
        assert_eq!(usage.output_tokens, 100);
        assert_eq!(usage.cache_read_input_tokens, 1_200);
        assert_eq!(usage.cache_creation_input_tokens, 300);
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
    fn split_input_prompt_cache_simulation_uses_mutually_exclusive_input() {
        let creation_only = CacheSimulation::from_prompt_cache_split_input(PromptCacheUsage {
            cache_creation_input_tokens: 40_000,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 40_000,
            cache_creation_1h_input_tokens: 0,
            effective_cache_ratio: None,
        })
        .unwrap()
        .to_usage(100_000, 1);

        assert_eq!(creation_only.total_input_tokens, 100_000);
        assert_eq!(creation_only.input_tokens, 60_000);
        assert_eq!(creation_only.cache_creation_input_tokens, 40_000);
        assert_eq!(creation_only.cache_read_input_tokens, 0);

        let read_and_creation = CacheSimulation::from_prompt_cache_split_input(PromptCacheUsage {
            cache_creation_input_tokens: 25_000,
            cache_read_input_tokens: 50_000,
            cache_creation_5m_input_tokens: 25_000,
            cache_creation_1h_input_tokens: 0,
            effective_cache_ratio: None,
        })
        .unwrap()
        .to_usage(100_000, 1);

        assert_eq!(read_and_creation.total_input_tokens, 100_000);
        assert_eq!(read_and_creation.input_tokens, 25_000);
        assert_eq!(read_and_creation.cache_creation_input_tokens, 25_000);
        assert_eq!(read_and_creation.cache_read_input_tokens, 50_000);
        assert_eq!(
            read_and_creation.input_tokens
                + read_and_creation.cache_creation_input_tokens
                + read_and_creation.cache_read_input_tokens,
            read_and_creation.total_input_tokens
        );

        let capped = CacheSimulation::from_prompt_cache_split_input(PromptCacheUsage {
            cache_creation_input_tokens: 160,
            cache_read_input_tokens: 30,
            cache_creation_5m_input_tokens: 160,
            cache_creation_1h_input_tokens: 0,
            effective_cache_ratio: None,
        })
        .unwrap()
        .to_usage(100, 1);

        assert_eq!(capped.total_input_tokens, 100);
        assert_eq!(capped.input_tokens, 0);
        assert_eq!(capped.cache_read_input_tokens, 0);
        assert_eq!(capped.cache_creation_input_tokens, 100);
        assert_eq!(
            capped.input_tokens
                + capped.cache_creation_input_tokens
                + capped.cache_read_input_tokens,
            capped.total_input_tokens
        );

        let ratio_split = CacheSimulation::from_prompt_cache_split_input(PromptCacheUsage {
            cache_creation_input_tokens: 60,
            cache_read_input_tokens: 40,
            cache_creation_5m_input_tokens: 60,
            cache_creation_1h_input_tokens: 0,
            effective_cache_ratio: Some(0.5),
        })
        .unwrap()
        .to_usage(100, 1);

        assert_eq!(ratio_split.total_input_tokens, 100);
        assert_eq!(ratio_split.input_tokens, 50);
        assert_eq!(ratio_split.cache_read_input_tokens, 20);
        assert_eq!(ratio_split.cache_creation_input_tokens, 30);
        assert_eq!(
            ratio_split.input_tokens
                + ratio_split.cache_creation_input_tokens
                + ratio_split.cache_read_input_tokens,
            ratio_split.total_input_tokens
        );
    }

    #[test]
    fn split_input_prompt_cache_simulation_applies_reported_input_range_with_jitter() {
        let mut inputs = std::collections::BTreeSet::new();
        for seed in 1..=48_u64 {
            let usage = CacheSimulation::from_prompt_cache_split_input_with_reported_input_range(
                PromptCacheUsage {
                    cache_creation_input_tokens: 20_000,
                    cache_read_input_tokens: 80_000,
                    cache_creation_5m_input_tokens: 20_000,
                    cache_creation_1h_input_tokens: 0,
                    effective_cache_ratio: None,
                },
                32,
                4_096,
                seed,
            )
            .unwrap()
            .to_usage(100_000, 1);

            assert!((32..=4_096).contains(&usage.input_tokens));
            assert_eq!(usage.cache_read_input_tokens, 80_000);
            assert!(usage.cache_creation_input_tokens < 20_000);
            assert_eq!(
                usage.input_tokens
                    + usage.cache_creation_input_tokens
                    + usage.cache_read_input_tokens,
                usage.total_input_tokens
            );
            inputs.insert(usage.input_tokens);
        }

        assert!(
            inputs.len() > 24,
            "range shaping should jitter instead of pinning to a threshold"
        );
        assert!(!inputs.contains(&0));
        assert!(!inputs.contains(&32));
        assert!(!inputs.contains(&4_096));
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
    fn simulation_parameters_change_ratio_scale_threshold_and_cap() {
        let prompt_usage = PromptCacheUsage {
            cache_creation_input_tokens: 40_000,
            cache_read_input_tokens: 60_000,
            cache_creation_5m_input_tokens: 40_000,
            cache_creation_1h_input_tokens: 0,
            effective_cache_ratio: None,
        };
        let low_ratio = CacheSimulation::from_prompt_cache_with_ratio_and_amplification(
            prompt_usage,
            0.25,
            None,
        )
        .unwrap()
        .to_usage(100_000, 1);
        let high_ratio = CacheSimulation::from_prompt_cache_with_ratio_and_amplification(
            prompt_usage,
            0.85,
            None,
        )
        .unwrap()
        .to_usage(100_000, 1);

        let low_cached = low_ratio.cache_read_input_tokens + low_ratio.cache_creation_input_tokens;
        let high_cached =
            high_ratio.cache_read_input_tokens + high_ratio.cache_creation_input_tokens;
        assert_eq!(low_cached, 25_000);
        assert_eq!(high_cached, 85_000);
        assert!(high_cached > low_cached);

        let thresholded = CacheAmplification::new(2.0, 0, 0, 0, 10_000, 0);
        assert_eq!(thresholded.apply(9_999), 9_999);
        assert_eq!(thresholded.apply(10_000), 20_000);

        let capped = CacheAmplification::new(3.0, 20_000, 1_000, 1_000, 0, 0);
        assert_eq!(capped.apply(40_000), 19_000);
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
    fn reported_usage_policy_samples_input_without_existing_read() {
        let usage = CacheUsage {
            total_input_tokens: 150_000,
            input_tokens: 100_000,
            output_tokens: 9,
            cache_creation_input_tokens: 50_000,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 50_000,
            cache_creation_1h_input_tokens: 0,
        };
        let raw = RawUsage::uncached(100_000, 9);
        let policy = ReportedCacheUsagePolicy::from_path_policy(
            ReportedUsagePathPolicy {
                input: ReportedUsageFieldPolicy::sample_input_max(96),
                ..ReportedUsagePathPolicy::default()
            },
            29,
        )
        .unwrap();

        let reported = usage.with_reported_cache_usage_policy_and_raw(policy, raw);

        assert!((1..=96).contains(&reported.input_tokens));
        assert_eq!(reported.cache_read_input_tokens, 0);
        assert_eq!(
            reported.cache_creation_input_tokens,
            50_000 + raw.input_tokens.saturating_sub(reported.input_tokens)
        );
        assert_eq!(
            reported.cache_creation_5m_input_tokens,
            50_000 + raw.input_tokens.saturating_sub(reported.input_tokens)
        );
        assert_eq!(reported.cache_creation_1h_input_tokens, 0);
        assert_eq!(
            reported.total_input_tokens,
            reported.reported_total_input_tokens()
        );
    }

    #[test]
    fn reported_usage_policy_uses_read_evidence_without_importing_read_value() {
        let usage = CacheUsage {
            total_input_tokens: 100_000,
            input_tokens: 100_000,
            output_tokens: 9,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        };
        let raw = RawUsage::uncached(100_000, 9);
        let policy = ReportedCacheUsagePolicy::input_only(96, 29).unwrap();

        let reported = usage.with_reported_cache_usage_policy_and_raw_evidence(policy, raw, true);

        assert!((1..=96).contains(&reported.input_tokens));
        assert_eq!(
            reported.cache_read_input_tokens,
            raw.input_tokens.saturating_sub(reported.input_tokens)
        );
        assert_eq!(
            reported.total_input_tokens,
            reported.reported_total_input_tokens()
        );
    }

    #[test]
    fn final_input_guard_samples_input_without_existing_read() {
        let usage = CacheUsage {
            total_input_tokens: 1_234,
            input_tokens: 1_234,
            output_tokens: 9,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        };
        let policy = ReportedCacheUsagePolicy::from_path_policy(
            ReportedUsagePathPolicy {
                input: ReportedUsageFieldPolicy::sample_input_max(96),
                ..ReportedUsagePathPolicy::default()
            },
            29,
        )
        .unwrap();

        let reported = policy.apply_final_input_guard(usage);

        assert!((1..=96).contains(&reported.input_tokens));
        assert_eq!(reported.cache_read_input_tokens, 0);
        assert_eq!(
            reported.cache_creation_input_tokens,
            usage.input_tokens.saturating_sub(reported.input_tokens)
        );
        assert_eq!(
            reported.cache_creation_5m_input_tokens,
            usage.input_tokens.saturating_sub(reported.input_tokens)
        );
        assert_eq!(reported.cache_creation_1h_input_tokens, 0);
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
    fn reported_usage_policy_caps_final_cache_creation_after_input_delta() {
        let usage = CacheUsage {
            total_input_tokens: 1_050_000,
            input_tokens: 1_020_000,
            output_tokens: 45,
            cache_creation_input_tokens: 30_000,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 30_000,
            cache_creation_1h_input_tokens: 0,
        };
        let raw = RawUsage::uncached(1_020_000, 45);
        let policy = ReportedCacheUsagePolicy::from_path_policy(
            ReportedUsagePathPolicy {
                final_cache_creation_max_tokens: 400_000,
                final_cache_creation_jitter_min_tokens: 0,
                final_cache_creation_jitter_max_tokens: 0,
                input: ReportedUsageFieldPolicy::sample_input_max(96),
                ..ReportedUsagePathPolicy::default()
            },
            71,
        )
        .unwrap();

        let reported = usage.with_reported_cache_usage_policy_and_raw(policy, raw);

        assert!((1..=96).contains(&reported.input_tokens));
        assert_eq!(reported.cache_read_input_tokens, 0);
        assert_eq!(reported.cache_creation_input_tokens, 400_000);
        assert_eq!(reported.cache_creation_5m_input_tokens, 400_000);
        assert_eq!(reported.cache_creation_1h_input_tokens, 0);
        assert_eq!(
            reported.total_input_tokens,
            reported.reported_total_input_tokens()
        );
    }

    #[test]
    fn reported_usage_policy_applies_deterministic_final_cache_creation_jitter() {
        let usage = CacheUsage {
            total_input_tokens: 1_050_000,
            input_tokens: 1_020_000,
            output_tokens: 45,
            cache_creation_input_tokens: 30_000,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 20_000,
            cache_creation_1h_input_tokens: 10_000,
        };
        let raw = RawUsage::uncached(1_020_000, 45);
        let policy = ReportedCacheUsagePolicy::from_path_policy(
            ReportedUsagePathPolicy {
                final_cache_creation_max_tokens: 400_000,
                final_cache_creation_jitter_min_tokens: 20_000,
                final_cache_creation_jitter_max_tokens: 45_000,
                input: ReportedUsageFieldPolicy::sample_input_max(96),
                ..ReportedUsagePathPolicy::default()
            },
            71,
        )
        .unwrap();

        let reported = usage.with_reported_cache_usage_policy_and_raw(policy.clone(), raw);
        let again = usage.with_reported_cache_usage_policy_and_raw(policy, raw);

        assert!((355_000..=380_000).contains(&reported.cache_creation_input_tokens));
        assert_eq!(
            reported.cache_creation_input_tokens,
            again.cache_creation_input_tokens
        );
        assert_eq!(
            reported.cache_creation_5m_input_tokens + reported.cache_creation_1h_input_tokens,
            reported.cache_creation_input_tokens
        );
        assert_eq!(
            reported.total_input_tokens,
            reported.reported_total_input_tokens()
        );
    }

    #[test]
    fn reported_usage_policy_final_cache_creation_guard_does_not_inflate_small_values() {
        let usage = CacheUsage {
            total_input_tokens: 250_050,
            input_tokens: 50,
            output_tokens: 9,
            cache_creation_input_tokens: 250_000,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 150_000,
            cache_creation_1h_input_tokens: 100_000,
        };
        let raw = RawUsage::uncached(50, 9);
        let policy = ReportedCacheUsagePolicy::from_path_policy(
            ReportedUsagePathPolicy {
                final_cache_creation_max_tokens: 400_000,
                final_cache_creation_jitter_min_tokens: 20_000,
                final_cache_creation_jitter_max_tokens: 45_000,
                ..ReportedUsagePathPolicy::default()
            },
            71,
        )
        .unwrap();

        let reported = usage.with_reported_cache_usage_policy_and_raw(policy, raw);

        assert_eq!(reported.cache_creation_input_tokens, 250_000);
        assert_eq!(reported.cache_creation_5m_input_tokens, 150_000);
        assert_eq!(reported.cache_creation_1h_input_tokens, 100_000);
        assert_eq!(reported.total_input_tokens, 250_050);
    }

    #[test]
    fn standard_cache_field_guard_caps_without_full_reported_usage_projection() {
        let usage = CacheUsage {
            total_input_tokens: 5_200_349,
            input_tokens: 349,
            output_tokens: 17,
            cache_creation_input_tokens: 2_500_000,
            cache_read_input_tokens: 2_700_000,
            cache_creation_5m_input_tokens: 1_600_000,
            cache_creation_1h_input_tokens: 900_000,
        };
        let policy = ReportedCacheUsagePolicy::from_path_policy(
            ReportedUsagePathPolicy {
                enabled: false,
                final_cache_read_max_tokens: 700_000,
                final_cache_creation_max_tokens: 400_000,
                final_cache_creation_jitter_min_tokens: 0,
                final_cache_creation_jitter_max_tokens: 0,
                ..ReportedUsagePathPolicy::default()
            },
            7,
        )
        .unwrap();

        assert_eq!(policy.apply_final_standard_cache_guards(usage), usage);
        let guarded = policy.apply_final_standard_cache_guards_for_standard_fields(usage);

        assert_eq!(guarded.input_tokens, 349);
        assert_eq!(guarded.output_tokens, 17);
        assert_eq!(guarded.cache_read_input_tokens, 700_000);
        assert_eq!(guarded.cache_creation_input_tokens, 400_000);
        assert_eq!(guarded.cache_creation_5m_input_tokens, 256_000);
        assert_eq!(guarded.cache_creation_1h_input_tokens, 144_000);
        assert_eq!(
            guarded.total_input_tokens,
            guarded.reported_total_input_tokens()
        );
    }

    #[test]
    fn reported_usage_path_policy_shapes_input_output_read_and_creation_fields() {
        let usage = CacheUsage {
            total_input_tokens: 760_000,
            input_tokens: 420_000,
            output_tokens: 12_000,
            cache_creation_input_tokens: 80_000,
            cache_read_input_tokens: 260_000,
            cache_creation_5m_input_tokens: 60_000,
            cache_creation_1h_input_tokens: 20_000,
        };
        let raw = RawUsage {
            input_tokens: 300_000,
            output_tokens: 8_000,
            cache_creation_input_tokens: 1_000,
            cache_read_input_tokens: 2_000,
            cache_creation_5m_input_tokens: 1_000,
            cache_creation_1h_input_tokens: 0,
        };
        let policy = ReportedCacheUsagePolicy::from_path_policy(
            ReportedUsagePathPolicy {
                final_cache_read_max_tokens: 180_000,
                input: ReportedUsageFieldPolicy::sample_input_max(96),
                output: ReportedUsageFieldPolicy::sample_target_with_multiplier(900, 1.2),
                cache_read: ReportedUsageFieldPolicy::sample_max(150_000),
                cache_creation: ReportedUsageFieldPolicy::sample_target_with_multiplier(6_000, 1.4),
                ..ReportedUsagePathPolicy::default()
            },
            131,
        )
        .unwrap();

        let reported = usage.with_reported_cache_usage_policy_and_raw(policy, raw);

        assert!((1..=96).contains(&reported.input_tokens));
        assert!((1..=1_080).contains(&reported.output_tokens));
        assert_eq!(reported.cache_read_input_tokens, 180_000);
        assert!((1..=8_400).contains(&reported.cache_creation_input_tokens));
        assert!(
            reported.cache_creation_5m_input_tokens + reported.cache_creation_1h_input_tokens
                <= reported.cache_creation_input_tokens
        );
        assert_ne!(reported.output_tokens, raw.output_tokens);
        assert_ne!(
            reported.cache_read_input_tokens,
            usage.cache_read_input_tokens
        );
        assert_eq!(
            reported.total_input_tokens,
            reported.reported_total_input_tokens()
        );
    }

    #[test]
    fn reported_usage_policy_applies_output_uplift_after_output_sampling() {
        let usage = CacheUsage {
            total_input_tokens: 50_000,
            input_tokens: 50_000,
            output_tokens: 4_000,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        };
        let base_policy = ReportedCacheUsagePolicy::from_path_policy(
            ReportedUsagePathPolicy {
                output: ReportedUsageFieldPolicy::sample_max(5_000),
                ..ReportedUsagePathPolicy::default()
            },
            131,
        )
        .unwrap();
        let uplift_policy = ReportedCacheUsagePolicy::from_path_policy(
            ReportedUsagePathPolicy {
                output: ReportedUsageFieldPolicy::sample_max(5_000),
                output_uplift_min_tokens: 1,
                output_uplift_percent: 50,
                ..ReportedUsagePathPolicy::default()
            },
            131,
        )
        .unwrap();

        let base = usage.with_reported_cache_usage_policy(base_policy);
        let uplifted = usage.with_reported_cache_usage_policy(uplift_policy);

        assert!(base.output_tokens > 1);
        assert_eq!(
            uplifted.output_tokens,
            uplift_tokens_by_percent(base.output_tokens, 50)
        );
    }

    #[test]
    fn reported_usage_policy_output_uplift_uses_strict_threshold() {
        let usage = CacheUsage {
            total_input_tokens: 50_000,
            input_tokens: 50_000,
            output_tokens: 1_000,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        };
        let policy = ReportedCacheUsagePolicy::from_path_policy(
            ReportedUsagePathPolicy {
                output_uplift_min_tokens: 1_000,
                output_uplift_percent: 50,
                ..ReportedUsagePathPolicy::default()
            },
            7,
        )
        .unwrap();

        let reported = usage.with_reported_cache_usage_policy(policy);

        assert_eq!(reported.output_tokens, 1_000);
    }

    #[test]
    fn reported_usage_policy_caps_output_after_uplift_with_jitter() {
        let usage = CacheUsage {
            total_input_tokens: 50_000,
            input_tokens: 50_000,
            output_tokens: 10_000,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        };
        let policy = ReportedCacheUsagePolicy::from_path_policy(
            ReportedUsagePathPolicy {
                output_uplift_min_tokens: 1_000,
                output_uplift_percent: 50,
                final_output_max_tokens: 12_000,
                final_output_jitter_min_tokens: 500,
                final_output_jitter_max_tokens: 500,
                ..ReportedUsagePathPolicy::default()
            },
            7,
        )
        .unwrap();

        let reported = usage.with_reported_cache_usage_policy(policy);

        assert_eq!(reported.output_tokens, 11_500);
    }

    #[test]
    fn reported_usage_policy_final_output_guard_can_be_disabled() {
        let usage = CacheUsage {
            total_input_tokens: 50_000,
            input_tokens: 50_000,
            output_tokens: 10_000,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        };
        let policy = ReportedCacheUsagePolicy::from_path_policy(
            ReportedUsagePathPolicy {
                final_output_guard_enabled: false,
                output_uplift_min_tokens: 1_000,
                output_uplift_percent: 50,
                final_output_max_tokens: 12_000,
                final_output_jitter_min_tokens: 500,
                final_output_jitter_max_tokens: 500,
                ..ReportedUsagePathPolicy::default()
            },
            7,
        )
        .unwrap();

        let reported = usage.with_reported_cache_usage_policy(policy);

        assert_eq!(reported.output_tokens, 10_000);
    }

    #[test]
    fn reported_usage_parameter_sweep_does_not_create_first_turn_cache_read() {
        let mut first_turn_read_violations = 0;
        let mut later_turn_delta_merges = 0;
        let mut sampled_creation_values = Vec::new();
        let input_caps = [64, 96, 500];
        let creation_targets = [3_000, 12_000, 30_000];

        for (scenario_idx, input_cap) in input_caps.iter().copied().enumerate() {
            for (target_idx, creation_target) in creation_targets.iter().copied().enumerate() {
                for group in 0..24 {
                    for turn in 0..15 {
                        let request_input = 80_000 + group * 1_337 + turn * 97;
                        let output_tokens = 8 + (turn % 5);
                        let usage = if turn == 0 {
                            CacheUsage {
                                total_input_tokens: request_input + 50_000,
                                input_tokens: request_input,
                                output_tokens,
                                cache_creation_input_tokens: 50_000,
                                cache_read_input_tokens: 0,
                                cache_creation_5m_input_tokens: 50_000,
                                cache_creation_1h_input_tokens: 0,
                            }
                        } else {
                            let read = 45_000 + turn * 1_000;
                            let creation = if turn % 4 == 0 { 12_000 } else { 0 };
                            CacheUsage {
                                total_input_tokens: request_input + read + creation,
                                input_tokens: request_input,
                                output_tokens,
                                cache_creation_input_tokens: creation,
                                cache_read_input_tokens: read,
                                cache_creation_5m_input_tokens: creation,
                                cache_creation_1h_input_tokens: 0,
                            }
                        };
                        let policy = ReportedCacheUsagePolicy::from_path_policy(
                            ReportedUsagePathPolicy {
                                final_cache_read_max_tokens: 700_000,
                                input: ReportedUsageFieldPolicy::sample_input_max(input_cap),
                                output: ReportedUsageFieldPolicy::sample_max(2_000),
                                cache_read: ReportedUsageFieldPolicy::preserve(),
                                cache_creation:
                                    ReportedUsageFieldPolicy::sample_target_with_multiplier(
                                        creation_target,
                                        1.2,
                                    ),
                                ..ReportedUsagePathPolicy::default()
                            },
                            scenario_idx as u64 * 10_000
                                + target_idx as u64 * 1_000
                                + group as u64 * 31
                                + turn as u64,
                        )
                        .unwrap();
                        let reported = usage.with_reported_cache_usage_policy_and_raw(
                            policy,
                            RawUsage::uncached(request_input, output_tokens),
                        );

                        assert!((1..=input_cap).contains(&reported.input_tokens));
                        assert_eq!(
                            reported.total_input_tokens,
                            reported.reported_total_input_tokens()
                        );
                        if turn == 0 {
                            if reported.cache_read_input_tokens > 0 {
                                first_turn_read_violations += 1;
                            }
                            assert_eq!(reported.cache_read_input_tokens, 0);
                        } else {
                            assert!(
                                reported.cache_read_input_tokens >= usage.cache_read_input_tokens
                            );
                            if reported.cache_read_input_tokens > usage.cache_read_input_tokens {
                                later_turn_delta_merges += 1;
                            }
                        }
                        if turn == 0 {
                            assert!(
                                reported.cache_creation_input_tokens
                                    >= request_input.saturating_sub(reported.input_tokens)
                            );
                        } else if reported.cache_creation_input_tokens > 0 {
                            sampled_creation_values.push(reported.cache_creation_input_tokens);
                            assert!(
                                reported.cache_creation_input_tokens
                                    <= ((creation_target as f64) * 1.2).round() as i32
                            );
                        }
                    }
                }
            }
        }

        let unique_creation_values = sampled_creation_values
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>()
            .len();
        println!(
            "CACHE_SWEEP reported_usage groups=216 turns_per_group=15 first_turn_read_violations={first_turn_read_violations} later_turn_delta_merges={later_turn_delta_merges} unique_creation_values={unique_creation_values}"
        );
        assert_eq!(first_turn_read_violations, 0);
        assert!(later_turn_delta_merges > 0);
        assert!(unique_creation_values > 60);
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
    fn reported_usage_policy_rewrites_records_when_only_final_cache_creation_guard_applies() {
        let usage = CacheUsage {
            total_input_tokens: 1_250_010,
            input_tokens: 10,
            output_tokens: 9,
            cache_creation_input_tokens: 1_250_000,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 900_000,
            cache_creation_1h_input_tokens: 350_000,
        };
        let policy = ReportedCacheUsagePolicy::from_path_policy(
            ReportedUsagePathPolicy {
                final_cache_creation_max_tokens: 1_000_000,
                final_cache_creation_jitter_min_tokens: 0,
                final_cache_creation_jitter_max_tokens: 0,
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
    fn reported_usage_policy_samples_creation_only_uncached_input() {
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

        assert!((1..=96).contains(&reported.input_tokens));
        assert_eq!(reported.cache_read_input_tokens, 0);
        let input_delta = usage.input_tokens.saturating_sub(reported.input_tokens);
        assert!(
            (input_delta.saturating_add(1)..=input_delta.saturating_add(3_300))
                .contains(&reported.cache_creation_input_tokens)
        );
        assert_eq!(
            reported.cache_creation_input_tokens,
            reported
                .cache_creation_5m_input_tokens
                .saturating_add(reported.cache_creation_1h_input_tokens)
        );
        assert_eq!(
            reported.total_input_tokens,
            reported.reported_total_input_tokens()
        );
    }

    #[test]
    fn input_only_reported_usage_policy_samples_input_without_cache_read() {
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

        let input_delta = usage.input_tokens.saturating_sub(reported.input_tokens);
        assert_eq!(reported.cache_creation_input_tokens, 40_000 + input_delta);
        assert_eq!(
            reported.cache_creation_5m_input_tokens,
            30_000 + input_delta
        );
        assert_eq!(reported.cache_creation_1h_input_tokens, 10_000);
        assert!((1..=96).contains(&reported.input_tokens));
        assert_eq!(reported.cache_read_input_tokens, 0);
        assert_eq!(
            reported.total_input_tokens,
            reported.reported_total_input_tokens()
        );
    }

    #[test]
    fn reported_usage_policy_samples_uncached_usage_without_cache_read() {
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

        assert!((1..=96).contains(&reported.input_tokens));
        assert_eq!(reported.cache_read_input_tokens, 0);
        assert_eq!(
            reported.cache_creation_input_tokens,
            usage.input_tokens.saturating_sub(reported.input_tokens)
        );
        assert_eq!(
            reported.cache_creation_5m_input_tokens,
            usage.input_tokens.saturating_sub(reported.input_tokens)
        );
        assert_eq!(reported.cache_creation_1h_input_tokens, 0);
        assert_eq!(reported.output_tokens, usage.output_tokens);
        assert_eq!(
            reported.total_input_tokens,
            reported.reported_total_input_tokens()
        );
    }

    #[test]
    fn reported_usage_policy_samples_uncached_input_naturally() {
        let usage = CacheUsage {
            total_input_tokens: 130_000,
            input_tokens: 80_000,
            output_tokens: 9,
            cache_creation_input_tokens: 40_000,
            cache_read_input_tokens: 10_000,
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

    fn hit_ratio(usage: CacheUsage) -> f64 {
        let total =
            usage.input_tokens + usage.cache_read_input_tokens + usage.cache_creation_input_tokens;
        usage.cache_read_input_tokens as f64 / total as f64
    }

    fn sonnet_pricing() -> ModelPricing {
        ModelPricing {
            input_cost_per_token: 0.000003,
            output_cost_per_token: 0.000015,
            cache_creation_input_token_cost: 0.00000375,
            cache_read_input_token_cost: 0.0000003,
        }
    }

    fn shaping(target: f64, min_tokens: i32, min_cost_ratio: f64) -> CacheHitShapingPolicy {
        CacheHitShapingPolicy {
            enabled: true,
            target_hit_ratio: target,
            ratio_jitter: 0.04,
            min_cache_read_tokens: min_tokens,
            min_cache_creation_tokens: min_tokens,
            min_cost_ratio,
            max_read_multiplier: 0.0,
            min_creation_ratio: 0.0,
        }
    }

    fn raw_input(tokens: i32, output: i32) -> CacheUsage {
        RawUsage::uncached(tokens, output).to_cache_usage()
    }

    fn input(policy: CacheHitShapingPolicy, raw: CacheUsage) -> CacheHitShapingInput {
        CacheHitShapingInput {
            policy,
            has_reusable_prefix: true,
            creation_allowed: true,
            raw,
            pricing: Some(sonnet_pricing()),
            input_min_tokens: 1,
            input_max_tokens: 20_000,
            max_cache_read_tokens: 0,
            max_cache_creation_tokens: 0,
            seed: 0,
        }
    }

    fn usage(input: i32, read: i32, creation: i32, output: i32) -> CacheUsage {
        CacheUsage {
            total_input_tokens: input + read + creation,
            input_tokens: input,
            output_tokens: output,
            cache_creation_input_tokens: creation,
            cache_read_input_tokens: read,
            cache_creation_5m_input_tokens: creation,
            cache_creation_1h_input_tokens: 0,
        }
    }

    fn sum(usage: CacheUsage) -> i32 {
        usage.input_tokens + usage.cache_read_input_tokens + usage.cache_creation_input_tokens
    }

    #[test]
    fn hit_shaping_reaches_target_keeps_total_and_writes() {
        // minea 放大后的典型请求：总量约 3.9 倍原始上下文。
        let base = usage(1_114, 23_463, 48_773, 78);
        let raw = raw_input(24_650, 78);
        for seed in 0..64 {
            let shaped = shape_cache_hit_usage(
                base,
                CacheHitShapingInput {
                    seed,
                    ..input(shaping(0.8, 1024, 1.0), raw)
                },
            );
            assert!(sum(shaped) >= sum(base), "seed {seed}");
            assert!(hit_ratio(shaped) >= 0.8 - 1e-6, "seed {seed}: {shaped:?}");
            assert!(
                shaped.cache_creation_input_tokens >= 1024,
                "writes kept: {shaped:?}"
            );
            assert!(
                sonnet_pricing().estimate(shaped) >= sonnet_pricing().estimate(raw) * (1.0 - 1e-9)
            );
            assert_eq!(
                shaped.cache_creation_5m_input_tokens + shaped.cache_creation_1h_input_tokens,
                shaped.cache_creation_input_tokens
            );
        }
    }

    #[test]
    fn hit_shaping_lowers_ratio_instead_of_losing_money() {
        // 总量只有 1 倍原始上下文时，90% 读取必然亏损，降到不亏损的最大占比。
        let base = usage(3_000, 20_000, 7_000, 10);
        let raw = raw_input(30_000, 10);
        let shaped = shape_cache_hit_usage(base, input(shaping(0.9, 1024, 1.0), raw));
        assert!(sum(shaped) >= 30_000);
        assert!(sonnet_pricing().estimate(shaped) >= sonnet_pricing().estimate(raw) * (1.0 - 1e-9));
        assert!(hit_ratio(shaped) >= 0.9 - 1e-6, "{shaped:?}");

        // 读取触及最终上限时，以计费下限为准，命中率可以低于目标。
        let shaped = shape_cache_hit_usage(
            base,
            CacheHitShapingInput {
                max_cache_read_tokens: 25_000,
                ..input(shaping(0.9, 1024, 1.0), raw)
            },
        );
        assert!(shaped.cache_read_input_tokens <= 25_000);
        assert!(sonnet_pricing().estimate(shaped) >= sonnet_pricing().estimate(raw) * (1.0 - 1e-9));
        assert!(hit_ratio(shaped) < 0.9);

        // 关闭计费下限时按目标值走。
        let shaped = shape_cache_hit_usage(base, input(shaping(0.9, 1024, 0.0), raw));
        assert!(hit_ratio(shaped) >= 0.9 - 1e-6);
    }

    #[test]
    fn hit_shaping_never_writes_when_creation_control_suppresses() {
        // 写入被 creation control 拦下的请求：最终写入必须为 0，计费仍不亏损。
        for (base, raw_tokens) in [
            (usage(3_599, 112_215, 0, 64), 29_836),
            (usage(20_000, 23_695, 4_298, 64), 27_410),
            (usage(1_114, 23_463, 48_773, 64), 24_650),
        ] {
            let raw = raw_input(raw_tokens, 64);
            let shaped = shape_cache_hit_usage(
                base,
                CacheHitShapingInput {
                    creation_allowed: false,
                    ..input(shaping(0.9, 1024, 1.0), raw)
                },
            );
            assert_eq!(
                shaped.cache_creation_input_tokens, 0,
                "{base:?} -> {shaped:?}"
            );
            assert!(
                sonnet_pricing().estimate(shaped) >= sonnet_pricing().estimate(raw) * (1.0 - 1e-9),
                "{base:?} -> {shaped:?}"
            );
        }
    }

    #[test]
    fn hit_shaping_does_not_invent_reads_without_reusable_prefix() {
        let base = usage(3_971, 0, 92_521, 101);
        let raw = raw_input(24_865, 101);
        let shaped = shape_cache_hit_usage(
            base,
            CacheHitShapingInput {
                has_reusable_prefix: false,
                ..input(shaping(0.9, 1024, 1.0), raw)
            },
        );
        assert_eq!(shaped.cache_read_input_tokens, 0);
        assert_eq!(shaped.cache_creation_input_tokens, 92_521);
    }

    #[test]
    fn hit_shaping_raises_small_cache_values() {
        for creation in [1, 6, 179, 1_023] {
            let base = usage(4_000, 90_000, creation, 9);
            let shaped = shape_cache_hit_usage(
                base,
                CacheHitShapingInput {
                    has_reusable_prefix: false,
                    ..input(shaping(0.0, 1024, 0.0), raw_input(10_000, 9))
                },
            );
            assert_eq!(
                shaped.cache_creation_input_tokens, 1024,
                "creation {creation}"
            );
        }
        let base = usage(5_000, 300, 0, 9);
        let shaped = shape_cache_hit_usage(
            base,
            CacheHitShapingInput {
                has_reusable_prefix: false,
                ..input(shaping(0.0, 1024, 0.0), raw_input(5_000, 9))
            },
        );
        assert_eq!(shaped.cache_read_input_tokens, 1024);
    }

    #[test]
    fn hit_shaping_never_outputs_small_values_or_loses_money() {
        let mut state = 0x1234_u64;
        for _ in 0..5_000 {
            state = splitmix64(state);
            let raw_tokens = ((splitmix64(state ^ 3) % 150_000) + 2) as i32;
            let scale = 1.0 + (state % 300) as f64 / 100.0;
            let total = ((raw_tokens as f64) * scale) as i32;
            let read = (splitmix64(state ^ 1) % (total as u64 + 1)) as i32
                * ((!state.is_multiple_of(3)) as i32);
            let creation = (splitmix64(state ^ 2) % ((total - read) as u64 + 1)) as i32;
            let inp = total - read - creation;
            let base = usage(inp, read, creation, 1);
            let target = (state % 100) as f64 / 100.0;
            let shaped = shape_cache_hit_usage(
                base,
                CacheHitShapingInput {
                    has_reusable_prefix: state.is_multiple_of(2),
                    creation_allowed: !state.is_multiple_of(5),
                    seed: state,
                    ..input(shaping(target, 1024, 1.0), raw_input(raw_tokens, 1))
                },
            );
            let r = shaped.cache_read_input_tokens;
            let c = shaped.cache_creation_input_tokens;
            if state.is_multiple_of(5) {
                assert_eq!(c, 0, "suppressed request wrote {c}: {base:?}");
            }
            assert!(r == 0 || r >= 1024, "read {r} from {base:?}");
            assert!(c == 0 || c >= 1024, "creation {c} from {base:?}");
            assert!(
                sonnet_pricing().estimate(shaped)
                    >= sonnet_pricing().estimate(raw_input(raw_tokens, 1)) * (1.0 - 1e-6),
                "loss: {base:?} -> {shaped:?} raw {raw_tokens}"
            );
        }
    }

    #[test]
    fn hit_shaping_read_multiplier_caps_reads_and_keeps_cost_floor() {
        let base = usage(1_114, 23_463, 48_773, 78);
        let raw = raw_input(24_650, 78);
        let policy = CacheHitShapingPolicy {
            max_read_multiplier: 4.0,
            ..shaping(0.95, 1024, 1.0)
        };
        let shaped = shape_cache_hit_usage(base, input(policy, raw));
        assert!(shaped.cache_read_input_tokens <= 98_600, "{shaped:?}");
        assert!(sonnet_pricing().estimate(shaped) >= sonnet_pricing().estimate(raw) * (1.0 - 1e-9));
        assert!(hit_ratio(shaped) < 0.95);

        // 0 表示不限制，可以达到目标。
        let shaped = shape_cache_hit_usage(base, input(shaping(0.95, 1024, 1.0), raw));
        assert!(hit_ratio(shaped) >= 0.95 - 1e-6);
    }

    #[test]
    fn hit_shaping_min_creation_ratio_raises_writes_when_allowed() {
        let base = usage(1_114, 23_463, 48_773, 78);
        let raw = raw_input(24_650, 78);
        let policy = CacheHitShapingPolicy {
            min_creation_ratio: 1.5,
            ..shaping(0.8, 1024, 1.0)
        };
        let shaped = shape_cache_hit_usage(base, input(policy, raw));
        assert!(shaped.cache_creation_input_tokens >= 36_975, "{shaped:?}");
        assert!(hit_ratio(shaped) >= 0.8 - 1e-6, "{shaped:?}");

        // 未放行写入时永远不产生写入：计费差额由 input 补足（允许超过 input 上限）。
        let base = usage(20_000, 148_206, 0, 64);
        let raw = raw_input(24_701, 64);
        let shaped = shape_cache_hit_usage(
            base,
            CacheHitShapingInput {
                creation_allowed: false,
                ..input(policy, raw)
            },
        );
        assert_eq!(shaped.cache_creation_input_tokens, 0, "{shaped:?}");
        assert!(sonnet_pricing().estimate(shaped) >= sonnet_pricing().estimate(raw) * (1.0 - 1e-9));

        // 未放行且计费已足够时不产生写入。
        let base = usage(3_599, 112_215, 0, 64);
        let shaped = shape_cache_hit_usage(
            base,
            CacheHitShapingInput {
                creation_allowed: false,
                ..input(policy, raw_input(2_000, 64))
            },
        );
        assert_eq!(shaped.cache_creation_input_tokens, 0);
    }

    #[test]
    fn hit_shaping_disabled_is_identity() {
        let base = usage(10_000, 0, 6, 1);
        let shaped = shape_cache_hit_usage(
            base,
            input(CacheHitShapingPolicy::disabled(), raw_input(10_000, 1)),
        );
        assert_eq!(shaped, base);
    }

    #[test]
    fn amplified_growth_reports_full_amplified_creation() {
        // 前缀增长时，放大后的差额整体记为写入，写入保持较大量级。
        let simulation = CacheSimulation::from_prompt_cache_with_ratio_and_amplification(
            PromptCacheUsage {
                cache_creation_input_tokens: 1_000,
                cache_read_input_tokens: 30_000,
                cache_creation_5m_input_tokens: 1_000,
                cache_creation_1h_input_tokens: 0,
                effective_cache_ratio: Some(0.99),
            },
            0.99,
            Some(CacheAmplification::new(3.0, 0, 0, 0, 3_000, 0)),
        )
        .unwrap();
        let usage = simulation.to_usage(31_400, 10);
        assert_eq!(usage.cache_read_input_tokens, 30_000);
        assert!(usage.cache_creation_input_tokens > 55_000, "{usage:?}");
    }
}
