//! Kiro-RS Tool Plus 缓存策略的下游 usage 整形。
//!
//! 输入是 Kiro-RS Tool 前缀追踪得到的拆分结果（`input + read + creation = 真实输入`），
//! 分两段执行，中间由调用方插入写入频次控制：
//!
//! 1. [`KiroPlusShaper::amplify`]：小请求关闭缓存、各字段按倍数放大并浮动；
//! 2. 写入频次控制（调用方用 `PromptCacheCreationController` 处理，被拦下的写入并回 input）；
//! 3. [`KiroPlusShaper::finalize`]：被拦写入改记读取 / 丢弃、读写上下限（可转入 input）、
//!    input / output 上下限、计费下限。
//!
//! 优先级：计费下限 > 上下限 > 放大倍数。

use crate::model::config::{
    KiroRsToolPlusCachePolicy, PromptCacheCreationControlConfig, SelectedUsageShaping,
    SuppressedCreationTarget, UsageFieldShaping, UsageShapingProfile,
};

use super::cache::{CacheUsage, normalize_shaped_cache_usage};
use super::pricing::ModelPricing;

const SALT_INPUT: u64 = 0x6a09_e667_f3bc_c908;
const SALT_OUTPUT: u64 = 0xbb67_ae85_84ca_a73b;
const SALT_READ: u64 = 0x3c6e_f372_fe94_f82b;
const SALT_CREATION: u64 = 0xa54f_f53a_5f1d_36f1;
const SALT_CAP: u64 = 0x510e_527f_ade6_82d1;

#[derive(Debug, Clone, Copy)]
pub(crate) struct KiroPlusShaper {
    policy: KiroRsToolPlusCachePolicy,
    selected: SelectedUsageShaping,
    small_request: bool,
    seed: u64,
}

impl KiroPlusShaper {
    pub(crate) fn new(
        policy: KiroRsToolPlusCachePolicy,
        stream: bool,
        raw_input_tokens: i32,
        seed: u64,
    ) -> Self {
        let policy = policy.normalized();
        Self {
            selected: policy.select_shaping(stream, raw_input_tokens),
            small_request: policy.is_small_request(raw_input_tokens),
            policy,
            seed,
        }
    }

    fn disables_cache(&self) -> bool {
        self.small_request && self.policy.small_request.disable_cache
    }

    /// 本次是否需要经过写入频次控制。不整形（raw）或小请求关闭缓存时跳过。
    pub(crate) fn frequency_control(&self) -> Option<PromptCacheCreationControlConfig> {
        let config = self.policy.creation_frequency.normalized();
        (config.enabled
            && !self.disables_cache()
            && !matches!(self.selected, SelectedUsageShaping::Raw))
        .then_some(config)
    }

    /// 第一段：关闭缓存（小请求）+ 各字段放大与浮动。
    pub(crate) fn amplify(&self, base: CacheUsage) -> CacheUsage {
        let mut usage = normalize_shaped_cache_usage(base);
        if self.disables_cache() {
            usage.input_tokens = usage
                .input_tokens
                .saturating_add(usage.cache_read_input_tokens)
                .saturating_add(usage.cache_creation_input_tokens);
            usage.cache_read_input_tokens = 0;
            usage.cache_creation_input_tokens = 0;
            usage.cache_creation_5m_input_tokens = 0;
            usage.cache_creation_1h_input_tokens = 0;
        }
        let SelectedUsageShaping::Profile(profile) = self.selected else {
            return normalize_shaped_cache_usage(usage);
        };

        let creation_before = usage.cache_creation_input_tokens;
        usage.input_tokens = scale(usage.input_tokens, profile.input, self.seed ^ SALT_INPUT);
        usage.output_tokens = scale(usage.output_tokens, profile.output, self.seed ^ SALT_OUTPUT);
        usage.cache_read_input_tokens = scale(
            usage.cache_read_input_tokens,
            profile.cache_read,
            self.seed ^ SALT_READ,
        );
        usage.cache_creation_input_tokens = scale(
            usage.cache_creation_input_tokens,
            profile.cache_creation,
            self.seed ^ SALT_CREATION,
        );
        rescale_creation_breakdown(&mut usage, creation_before);
        normalize_shaped_cache_usage(usage)
    }

    /// 第二段：在写入频次控制之后执行上下限与计费下限。
    ///
    /// `amplified` 是第一段结果，`controlled` 是频次控制后的结果（未开启时两者相同），
    /// `raw` 是真实输入（计费下限的基准）。
    pub(crate) fn finalize(
        &self,
        amplified: CacheUsage,
        controlled: CacheUsage,
        pricing: Option<ModelPricing>,
        raw: CacheUsage,
    ) -> CacheUsage {
        let SelectedUsageShaping::Profile(profile) = self.selected else {
            return normalize_shaped_cache_usage(controlled);
        };
        let mut usage = controlled;
        redirect_suppressed_creation(
            &mut usage,
            amplified,
            self.policy.suppressed_creation_target,
        );

        let read_overflow = bound_cache_field(
            &mut usage.cache_read_input_tokens,
            profile.cache_read,
            self.seed ^ SALT_READ ^ SALT_CAP,
        );
        let creation_before = usage.cache_creation_input_tokens;
        let creation_overflow = bound_cache_field(
            &mut usage.cache_creation_input_tokens,
            profile.cache_creation,
            self.seed ^ SALT_CREATION ^ SALT_CAP,
        );
        if usage.cache_creation_input_tokens != creation_before {
            rescale_creation_breakdown(&mut usage, creation_before);
        }
        if profile.cap_overflow_to_input {
            usage.input_tokens = usage
                .input_tokens
                .saturating_add(read_overflow)
                .saturating_add(creation_overflow);
        }

        bound_field(
            &mut usage.input_tokens,
            profile.input,
            self.seed ^ SALT_INPUT ^ SALT_CAP,
        );
        bound_field(
            &mut usage.output_tokens,
            profile.output,
            self.seed ^ SALT_OUTPUT ^ SALT_CAP,
        );
        apply_cost_floor(&mut usage, profile, pricing, raw);
        normalize_shaped_cache_usage(usage)
    }

    /// 不经过写入频次控制的完整整形（测试与无状态场景使用）。
    #[cfg(test)]
    pub(crate) fn shape(
        &self,
        base: CacheUsage,
        pricing: Option<ModelPricing>,
        raw: CacheUsage,
    ) -> CacheUsage {
        let amplified = self.amplify(base);
        self.finalize(amplified, amplified, pricing, raw)
    }
}

/// 频次控制把被拦下的写入并回了 input；按配置改记为读取或丢弃。
fn redirect_suppressed_creation(
    usage: &mut CacheUsage,
    amplified: CacheUsage,
    target: SuppressedCreationTarget,
) {
    let suppressed = amplified
        .cache_creation_input_tokens
        .saturating_sub(usage.cache_creation_input_tokens)
        .max(0);
    if suppressed == 0 {
        return;
    }
    let moved = suppressed.min(
        usage
            .input_tokens
            .saturating_sub(amplified.input_tokens)
            .max(0),
    );
    match target {
        SuppressedCreationTarget::Input => {}
        SuppressedCreationTarget::CacheRead => {
            usage.input_tokens -= moved;
            usage.cache_read_input_tokens = usage.cache_read_input_tokens.saturating_add(moved);
        }
        SuppressedCreationTarget::Drop => {
            usage.input_tokens -= moved;
        }
    }
}

fn scale(value: i32, shaping: UsageFieldShaping, seed: u64) -> i32 {
    let value = value.max(0);
    if value == 0 {
        return 0;
    }
    let mut factor = shaping.multiplier;
    if shaping.jitter_ratio > 0.0 {
        factor *= 1.0 + shaping.jitter_ratio * (2.0 * unit(seed) - 1.0);
    }
    if (factor - 1.0).abs() < f64::EPSILON {
        return value;
    }
    ((value as f64) * factor.max(0.0))
        .round()
        .clamp(0.0, i32::MAX as f64) as i32
}

/// 有效上限：`max - [0, maxJitterTokens]`，至少为 1。0 表示不限。
fn effective_max(shaping: UsageFieldShaping, seed: u64) -> i32 {
    if shaping.max_tokens <= 0 {
        return i32::MAX;
    }
    let jitter = shaping
        .max_jitter_tokens
        .min(shaping.max_tokens.saturating_sub(1))
        .max(0);
    if jitter == 0 {
        return shaping.max_tokens;
    }
    let deduction = (splitmix64(seed) % (jitter as u64 + 1)) as i32;
    (shaping.max_tokens - deduction).max(1)
}

/// 非零值抬到下限、裁到上限；返回被上限裁掉的量。
fn bound_cache_field(value: &mut i32, shaping: UsageFieldShaping, seed: u64) -> i32 {
    let current = (*value).max(0);
    if current == 0 {
        *value = 0;
        return 0;
    }
    let max = effective_max(shaping, seed);
    let bounded = current.max(shaping.min_tokens).min(max);
    *value = bounded;
    current.saturating_sub(bounded).max(0)
}

fn bound_field(value: &mut i32, shaping: UsageFieldShaping, seed: u64) {
    bound_cache_field(value, shaping, seed);
}

/// 写入量变化后按原有 5m / 1h 比例重新分配。
fn rescale_creation_breakdown(usage: &mut CacheUsage, creation_before: i32) {
    let creation = usage.cache_creation_input_tokens.max(0);
    let total = usage
        .cache_creation_5m_input_tokens
        .max(0)
        .saturating_add(usage.cache_creation_1h_input_tokens.max(0));
    if creation == 0 {
        usage.cache_creation_5m_input_tokens = 0;
        usage.cache_creation_1h_input_tokens = 0;
        return;
    }
    if total <= 0 || creation_before <= 0 {
        // 没有可参考的比例：由 normalize_shaped_cache_usage 归入 5m。
        usage.cache_creation_5m_input_tokens = 0;
        usage.cache_creation_1h_input_tokens = 0;
        return;
    }
    let one_hour = ((usage.cache_creation_1h_input_tokens.max(0) as i64) * (creation as i64)
        / (total as i64)) as i32;
    usage.cache_creation_1h_input_tokens = one_hour.clamp(0, creation);
    usage.cache_creation_5m_input_tokens = creation - usage.cache_creation_1h_input_tokens;
}

fn apply_cost_floor(
    usage: &mut CacheUsage,
    profile: UsageShapingProfile,
    pricing: Option<ModelPricing>,
    raw: CacheUsage,
) {
    if profile.min_cost_ratio <= 0.0 {
        return;
    }
    let Some(pricing) = pricing else {
        return;
    };
    if pricing.input_cost_per_token <= 0.0 {
        return;
    }
    let floor = pricing.estimate(raw) * profile.min_cost_ratio;
    let current = pricing.estimate(*usage);
    if current >= floor * (1.0 - 1e-9) {
        return;
    }
    let tokens = ((floor - current) / pricing.input_cost_per_token)
        .ceil()
        .clamp(0.0, i32::MAX as f64) as i32;
    usage.input_tokens = usage.input_tokens.saturating_add(tokens);
}

fn unit(seed: u64) -> f64 {
    (splitmix64(seed) % 10_001) as f64 / 10_000.0
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::kiro_rs_tool_plus::UsageShapingProfileMode;

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

    fn raw(input: i32, output: i32) -> CacheUsage {
        usage(input, 0, 0, output)
    }

    fn shaper(policy: KiroRsToolPlusCachePolicy, stream: bool, raw_input: i32) -> KiroPlusShaper {
        KiroPlusShaper::new(policy, stream, raw_input, 42)
    }

    #[test]
    fn default_policy_keeps_split_unchanged() {
        let base = usage(100, 9_000, 900, 50);
        let shaped = shaper(KiroRsToolPlusCachePolicy::default(), true, 10_000).shape(
            base,
            None,
            raw(10_000, 50),
        );
        assert_eq!(shaped, normalize_shaped_cache_usage(base));
    }

    #[test]
    fn amplifies_each_field_independently() {
        let mut policy = KiroRsToolPlusCachePolicy::default();
        policy.shaping.cache_read.multiplier = 3.0;
        policy.shaping.cache_creation.multiplier = 2.0;
        policy.shaping.input.multiplier = 1.5;
        policy.shaping.output.multiplier = 0.5;
        let shaped =
            shaper(policy, true, 10_000).shape(usage(100, 9_000, 900, 50), None, raw(10_000, 50));
        assert_eq!(shaped.cache_read_input_tokens, 27_000);
        assert_eq!(shaped.cache_creation_input_tokens, 1_800);
        assert_eq!(shaped.input_tokens, 150);
        assert_eq!(shaped.output_tokens, 25);
        assert_eq!(shaped.total_input_tokens, 150 + 27_000 + 1_800);
        assert_eq!(
            shaped.cache_creation_5m_input_tokens + shaped.cache_creation_1h_input_tokens,
            1_800
        );
    }

    #[test]
    fn jitter_stays_within_ratio_and_is_deterministic() {
        let mut policy = KiroRsToolPlusCachePolicy::default();
        policy.shaping.cache_read.multiplier = 2.0;
        policy.shaping.cache_read.jitter_ratio = 0.1;
        for seed in 0..200_u64 {
            let s = KiroPlusShaper::new(policy, true, 10_000, seed);
            let a = s.shape(usage(100, 10_000, 0, 1), None, raw(10_100, 1));
            let b = s.shape(usage(100, 10_000, 0, 1), None, raw(10_100, 1));
            assert_eq!(a, b);
            assert!((18_000..=22_000).contains(&a.cache_read_input_tokens));
        }
    }

    #[test]
    fn bounds_apply_after_amplification_and_overflow_can_move_to_input() {
        let mut policy = KiroRsToolPlusCachePolicy::default();
        policy.shaping.cache_read.multiplier = 10.0;
        policy.shaping.cache_read.max_tokens = 50_000;
        policy.shaping.cache_creation.min_tokens = 2_048;
        policy.shaping.output.min_tokens = 100;
        let shaped =
            shaper(policy, true, 10_000).shape(usage(100, 9_000, 900, 50), None, raw(10_000, 50));
        assert_eq!(shaped.cache_read_input_tokens, 50_000);
        assert_eq!(shaped.cache_creation_input_tokens, 2_048);
        assert_eq!(shaped.output_tokens, 100);
        assert_eq!(shaped.input_tokens, 100);

        policy.shaping.cap_overflow_to_input = true;
        let shaped =
            shaper(policy, true, 10_000).shape(usage(100, 9_000, 900, 50), None, raw(10_000, 50));
        assert_eq!(shaped.input_tokens, 100 + 40_000);
    }

    #[test]
    fn min_does_not_raise_zero_cache_fields() {
        let mut policy = KiroRsToolPlusCachePolicy::default();
        policy.shaping.cache_read.min_tokens = 1_024;
        policy.shaping.cache_creation.min_tokens = 1_024;
        let shaped = shaper(policy, true, 500).shape(usage(500, 0, 0, 10), None, raw(500, 10));
        assert_eq!(shaped.cache_read_input_tokens, 0);
        assert_eq!(shaped.cache_creation_input_tokens, 0);
    }

    #[test]
    fn max_jitter_deducts_within_range() {
        let mut policy = KiroRsToolPlusCachePolicy::default();
        policy.shaping.cache_read.max_tokens = 10_000;
        policy.shaping.cache_read.max_jitter_tokens = 500;
        let mut seen = std::collections::BTreeSet::new();
        for seed in 0..100_u64 {
            let shaped = KiroPlusShaper::new(policy, true, 100_000, seed).shape(
                usage(100, 90_000, 0, 1),
                None,
                raw(90_100, 1),
            );
            assert!((9_500..=10_000).contains(&shaped.cache_read_input_tokens));
            seen.insert(shaped.cache_read_input_tokens);
        }
        assert!(seen.len() > 1);
    }

    #[test]
    fn non_stream_and_small_request_profiles() {
        let mut policy = KiroRsToolPlusCachePolicy::default();
        policy.shaping.cache_read.multiplier = 2.0;
        policy.non_stream.mode = UsageShapingProfileMode::Custom;
        policy.non_stream.profile.cache_read.multiplier = 4.0;
        policy.small_request.enabled = true;
        policy.small_request.max_input_tokens = 2_000;
        policy.small_request.disable_cache = true;

        let base = usage(100, 9_000, 900, 50);
        let stream = shaper(policy, true, 10_000).shape(base, None, raw(10_000, 50));
        assert_eq!(stream.cache_read_input_tokens, 18_000);
        let non_stream = shaper(policy, false, 10_000).shape(base, None, raw(10_000, 50));
        assert_eq!(non_stream.cache_read_input_tokens, 36_000);

        let small =
            shaper(policy, false, 1_500).shape(usage(100, 1_000, 400, 5), None, raw(1_500, 5));
        assert_eq!(small.cache_read_input_tokens, 0);
        assert_eq!(small.cache_creation_input_tokens, 0);
        assert_eq!(small.input_tokens, 1_500);
        assert!(shaper(policy, false, 1_500).frequency_control().is_none());

        policy.non_stream.mode = UsageShapingProfileMode::Raw;
        let raw_mode = shaper(policy, false, 10_000).shape(base, None, raw(10_000, 50));
        assert_eq!(raw_mode, normalize_shaped_cache_usage(base));
    }

    #[test]
    fn suppressed_creation_can_become_read_or_be_dropped() {
        let mut policy = KiroRsToolPlusCachePolicy::default();
        let amplified = usage(100, 9_000, 900, 50);
        // 频次控制拦下全部写入：写入并回 input。
        let controlled = usage(1_000, 9_000, 0, 50);

        let s = shaper(policy, true, 10_000);
        assert_eq!(
            s.finalize(amplified, controlled, None, raw(10_000, 50))
                .input_tokens,
            1_000
        );

        policy.suppressed_creation_target = SuppressedCreationTarget::CacheRead;
        let shaped =
            shaper(policy, true, 10_000).finalize(amplified, controlled, None, raw(10_000, 50));
        assert_eq!(shaped.input_tokens, 100);
        assert_eq!(shaped.cache_read_input_tokens, 9_900);

        policy.suppressed_creation_target = SuppressedCreationTarget::Drop;
        let shaped =
            shaper(policy, true, 10_000).finalize(amplified, controlled, None, raw(10_000, 50));
        assert_eq!(shaped.input_tokens, 100);
        assert_eq!(shaped.cache_read_input_tokens, 9_000);
        assert_eq!(shaped.cache_creation_input_tokens, 0);
    }

    #[test]
    fn cost_floor_tops_up_input_beyond_input_cap() {
        let pricing = ModelPricing {
            input_cost_per_token: 3.0,
            output_cost_per_token: 15.0,
            cache_creation_input_token_cost: 3.75,
            cache_creation_1h_input_token_cost: 0.0,
            cache_read_input_token_cost: 0.3,
        };
        let mut policy = KiroRsToolPlusCachePolicy::default();
        policy.shaping.input.max_tokens = 100;
        policy.shaping.min_cost_ratio = 1.0;
        let raw_usage = raw(10_000, 50);
        let shaped =
            shaper(policy, true, 10_000).shape(usage(100, 9_900, 0, 50), Some(pricing), raw_usage);
        assert!(pricing.estimate(shaped) >= pricing.estimate(raw_usage) * (1.0 - 1e-9));
        assert!(shaped.input_tokens > 100);
    }
}
