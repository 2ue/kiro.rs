//! Kiro-RS Tool Plus 缓存策略配置。
//!
//! 在 Kiro-RS Tool 的本地前缀追踪基础上，对下游 usage 的四个字段（input / output /
//! cache_read / cache_creation）分别做放大、浮动和上下限控制，并接入写入频次控制。
//! 流式、非流式和小请求可以使用不同的整形档位。
//!
//! 全部字段带默认值，默认行为与 Kiro-RS Tool 的前缀拆分一致（不放大、不限制）。

use serde::{Deserialize, Serialize};

use super::config::{KiroRsToolCachePolicy, PromptCacheCreationControlConfig};

/// 单个字段倍数上限。
pub const MAX_FIELD_MULTIPLIER: f64 = 1_000.0;
/// 单个字段浮动比例上限（±100%）。
pub const MAX_FIELD_JITTER_RATIO: f64 = 1.0;
/// 计费下限倍数上限。
pub const MAX_MIN_COST_RATIO: f64 = 100.0;

/// 单个 usage 字段的整形参数。
///
/// 执行顺序：`值 × multiplier × (1 ± jitterRatio)` → 非零时抬到 `minTokens` →
/// 裁到 `maxTokens - [0, maxJitterTokens]`。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageFieldShaping {
    /// 放大倍数，可以大于 1；0 表示该字段上报为 0。范围 0~1000，默认 1。
    #[serde(default = "default_multiplier")]
    pub multiplier: f64,
    /// 放大后的确定性浮动比例，结果落在 `×(1 - r) ~ ×(1 + r)`。范围 0~1，默认 0。
    #[serde(default)]
    pub jitter_ratio: f64,
    /// 非零值的下限，0 表示不限。字段本身为 0（未命中 / 未写入）时不抬高。
    #[serde(default)]
    pub min_tokens: i32,
    /// 上限，0 表示不限。
    #[serde(default)]
    pub max_tokens: i32,
    /// 触及上限时的确定性扣减量上限，避免每次都精确等于上限。0 表示不扣减。
    #[serde(default)]
    pub max_jitter_tokens: i32,
}

impl Default for UsageFieldShaping {
    fn default() -> Self {
        Self {
            multiplier: 1.0,
            jitter_ratio: 0.0,
            min_tokens: 0,
            max_tokens: 0,
            max_jitter_tokens: 0,
        }
    }
}

impl UsageFieldShaping {
    pub fn normalized(mut self) -> Self {
        if !self.multiplier.is_finite() {
            self.multiplier = 1.0;
        }
        self.multiplier = self.multiplier.clamp(0.0, MAX_FIELD_MULTIPLIER);
        if !self.jitter_ratio.is_finite() {
            self.jitter_ratio = 0.0;
        }
        self.jitter_ratio = self.jitter_ratio.clamp(0.0, MAX_FIELD_JITTER_RATIO);
        self.min_tokens = self.min_tokens.max(0);
        self.max_tokens = self.max_tokens.max(0);
        if self.max_tokens > 0 && self.min_tokens > self.max_tokens {
            self.min_tokens = self.max_tokens;
        }
        self.max_jitter_tokens = self.max_jitter_tokens.max(0);
        self
    }

    pub fn validate(&self, label: &str) -> Result<(), String> {
        if !self.multiplier.is_finite() || !(0.0..=MAX_FIELD_MULTIPLIER).contains(&self.multiplier)
        {
            return Err(format!("{label}.multiplier 必须在 0 到 1000 之间"));
        }
        if !self.jitter_ratio.is_finite()
            || !(0.0..=MAX_FIELD_JITTER_RATIO).contains(&self.jitter_ratio)
        {
            return Err(format!("{label}.jitterRatio 必须在 0 到 1 之间"));
        }
        if self.min_tokens < 0 {
            return Err(format!("{label}.minTokens 不能小于 0"));
        }
        if self.max_tokens < 0 {
            return Err(format!("{label}.maxTokens 不能小于 0"));
        }
        if self.max_jitter_tokens < 0 {
            return Err(format!("{label}.maxJitterTokens 不能小于 0"));
        }
        if self.max_tokens > 0 && self.min_tokens > self.max_tokens {
            return Err(format!("{label}.minTokens 不能大于 maxTokens"));
        }
        Ok(())
    }
}

/// 一组完整的 usage 整形参数。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct UsageShapingProfile {
    #[serde(default)]
    pub input: UsageFieldShaping,
    #[serde(default)]
    pub output: UsageFieldShaping,
    #[serde(default)]
    pub cache_read: UsageFieldShaping,
    #[serde(default)]
    pub cache_creation: UsageFieldShaping,
    /// 缓存读取 / 写入被上限裁掉的部分是否转入 input。默认丢弃。
    #[serde(default)]
    pub cap_overflow_to_input: bool,
    /// 计费下限：整形后计费不低于真实输入计费的这个倍数，差额补到 input（不受 input 上限约束）。
    /// 范围 0~100，0 表示关闭。
    #[serde(default)]
    pub min_cost_ratio: f64,
}

impl UsageShapingProfile {
    pub fn normalized(mut self) -> Self {
        self.input = self.input.normalized();
        self.output = self.output.normalized();
        self.cache_read = self.cache_read.normalized();
        self.cache_creation = self.cache_creation.normalized();
        if !self.min_cost_ratio.is_finite() {
            self.min_cost_ratio = 0.0;
        }
        self.min_cost_ratio = self.min_cost_ratio.clamp(0.0, MAX_MIN_COST_RATIO);
        self
    }

    pub fn validate(&self, label: &str) -> Result<(), String> {
        self.input.validate(&format!("{label}.input"))?;
        self.output.validate(&format!("{label}.output"))?;
        self.cache_read.validate(&format!("{label}.cacheRead"))?;
        self.cache_creation
            .validate(&format!("{label}.cacheCreation"))?;
        if !self.min_cost_ratio.is_finite()
            || !(0.0..=MAX_MIN_COST_RATIO).contains(&self.min_cost_ratio)
        {
            return Err(format!("{label}.minCostRatio 必须在 0 到 100 之间"));
        }
        Ok(())
    }
}

/// 非流式 / 小请求使用哪套整形参数。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum UsageShapingProfileMode {
    /// 沿用默认整形参数。
    #[default]
    Inherit,
    /// 使用本节自己的整形参数。
    Custom,
    /// 不整形：上报前缀追踪的原始拆分结果，也不经过写入频次控制。
    Raw,
}

/// 非流式请求的整形。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct NonStreamShapingPolicy {
    #[serde(default)]
    pub mode: UsageShapingProfileMode,
    #[serde(default)]
    pub profile: UsageShapingProfile,
}

/// 小请求整形：真实输入不超过阈值的请求。优先级高于非流式整形。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SmallRequestShapingPolicy {
    #[serde(default)]
    pub enabled: bool,
    /// 真实输入 tokens 不超过该值视为小请求。
    #[serde(default = "default_small_request_max_input_tokens")]
    pub max_input_tokens: i32,
    /// 小请求不上报缓存：读取 / 写入全部并回 input，也不消耗写入频次额度。
    #[serde(default)]
    pub disable_cache: bool,
    #[serde(default)]
    pub mode: UsageShapingProfileMode,
    #[serde(default)]
    pub profile: UsageShapingProfile,
}

impl Default for SmallRequestShapingPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            max_input_tokens: default_small_request_max_input_tokens(),
            disable_cache: false,
            mode: UsageShapingProfileMode::Inherit,
            profile: UsageShapingProfile::default(),
        }
    }
}

/// 被写入频次控制拦下的写入记到哪里。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SuppressedCreationTarget {
    /// 记为 input（与现有频次控制一致）。
    #[default]
    Input,
    /// 记为缓存读取。
    CacheRead,
    /// 直接丢弃。
    Drop,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct KiroRsToolPlusCachePolicy {
    // ---- 前缀追踪（同 Kiro-RS Tool） ----
    #[serde(default = "default_coverage_ratio")]
    pub coverage_ratio: f64,
    #[serde(default)]
    pub max_coverage_tokens: i32,
    #[serde(default = "default_true")]
    pub incremental_create_enabled: bool,
    #[serde(default)]
    pub cache_current_user_stable_prefix: bool,
    #[serde(default)]
    pub current_user_stable_prefix_max_tokens: i32,

    // ---- 写入频次 ----
    /// 写入频次控制，参数含义与全局 creation control 相同；默认关闭。
    /// 在放大之后、上下限之前执行，额度按放大后的写入计算。
    #[serde(default = "default_creation_frequency")]
    pub creation_frequency: PromptCacheCreationControlConfig,
    #[serde(default)]
    pub suppressed_creation_target: SuppressedCreationTarget,

    // ---- 整形 ----
    /// 默认整形参数（流式请求，以及选择 inherit 的非流式 / 小请求）。
    #[serde(default)]
    pub shaping: UsageShapingProfile,
    #[serde(default)]
    pub non_stream: NonStreamShapingPolicy,
    #[serde(default)]
    pub small_request: SmallRequestShapingPolicy,
}

impl Default for KiroRsToolPlusCachePolicy {
    fn default() -> Self {
        Self {
            coverage_ratio: default_coverage_ratio(),
            max_coverage_tokens: 0,
            incremental_create_enabled: true,
            cache_current_user_stable_prefix: false,
            current_user_stable_prefix_max_tokens: 0,
            creation_frequency: default_creation_frequency(),
            suppressed_creation_target: SuppressedCreationTarget::default(),
            shaping: UsageShapingProfile::default(),
            non_stream: NonStreamShapingPolicy::default(),
            small_request: SmallRequestShapingPolicy::default(),
        }
    }
}

/// 一次请求实际选中的整形方式。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SelectedUsageShaping {
    Raw,
    Profile(UsageShapingProfile),
}

impl KiroRsToolPlusCachePolicy {
    pub fn normalized(mut self) -> Self {
        if !self.coverage_ratio.is_finite() {
            self.coverage_ratio = default_coverage_ratio();
        }
        self.coverage_ratio = self.coverage_ratio.clamp(0.0, 1.0);
        self.max_coverage_tokens = self.max_coverage_tokens.max(0);
        self.current_user_stable_prefix_max_tokens =
            self.current_user_stable_prefix_max_tokens.max(0);
        if !self.cache_current_user_stable_prefix {
            self.current_user_stable_prefix_max_tokens = 0;
        }
        self.creation_frequency = self.creation_frequency.normalized();
        self.shaping = self.shaping.normalized();
        self.non_stream.profile = self.non_stream.profile.normalized();
        self.small_request.max_input_tokens = self.small_request.max_input_tokens.max(0);
        self.small_request.profile = self.small_request.profile.normalized();
        self
    }

    pub fn validate(&self, label: &str) -> Result<(), String> {
        if !self.coverage_ratio.is_finite() || !(0.0..=1.0).contains(&self.coverage_ratio) {
            return Err(format!("{label}.coverageRatio 必须在 0 到 1 之间"));
        }
        if self.max_coverage_tokens < 0 {
            return Err(format!("{label}.maxCoverageTokens 不能小于 0"));
        }
        if self.current_user_stable_prefix_max_tokens < 0 {
            return Err(format!(
                "{label}.currentUserStablePrefixMaxTokens 不能小于 0"
            ));
        }
        self.creation_frequency
            .validate()
            .map_err(|err| format!("{label}.creationFrequency: {err}"))?;
        self.shaping.validate(&format!("{label}.shaping"))?;
        self.non_stream
            .profile
            .validate(&format!("{label}.nonStream.profile"))?;
        if self.small_request.max_input_tokens < 0 {
            return Err(format!("{label}.smallRequest.maxInputTokens 不能小于 0"));
        }
        self.small_request
            .profile
            .validate(&format!("{label}.smallRequest.profile"))?;
        Ok(())
    }

    /// 前缀追踪参数，复用 Kiro-RS Tool 的追踪实现。input 范围由本策略的整形参数控制，
    /// 这里不再采样。
    pub fn tracker_policy(&self) -> KiroRsToolCachePolicy {
        KiroRsToolCachePolicy {
            coverage_ratio: self.coverage_ratio,
            max_coverage_tokens: self.max_coverage_tokens,
            incremental_create_enabled: self.incremental_create_enabled,
            max_new_creation_tokens_per_request: 0,
            cache_current_user_stable_prefix: self.cache_current_user_stable_prefix,
            current_user_stable_prefix_max_tokens: self.current_user_stable_prefix_max_tokens,
            reported_input_min_tokens: 0,
            reported_input_max_tokens: 0,
        }
        .normalized()
    }

    pub fn is_small_request(&self, raw_input_tokens: i32) -> bool {
        self.small_request.enabled && raw_input_tokens <= self.small_request.max_input_tokens
    }

    /// 优先级：小请求 > 非流式 > 默认。
    pub fn select_shaping(&self, stream: bool, raw_input_tokens: i32) -> SelectedUsageShaping {
        let section = if self.is_small_request(raw_input_tokens) {
            Some((self.small_request.mode, self.small_request.profile))
        } else if !stream {
            Some((self.non_stream.mode, self.non_stream.profile))
        } else {
            None
        };
        match section {
            Some((UsageShapingProfileMode::Raw, _)) => SelectedUsageShaping::Raw,
            Some((UsageShapingProfileMode::Custom, profile)) => {
                SelectedUsageShaping::Profile(profile.normalized())
            }
            Some((UsageShapingProfileMode::Inherit, _)) | None => {
                SelectedUsageShaping::Profile(self.shaping.normalized())
            }
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_multiplier() -> f64 {
    1.0
}

fn default_coverage_ratio() -> f64 {
    1.0
}

fn default_small_request_max_input_tokens() -> i32 {
    1_024
}

fn default_creation_frequency() -> PromptCacheCreationControlConfig {
    PromptCacheCreationControlConfig {
        enabled: false,
        ..PromptCacheCreationControlConfig::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_deserialize_from_empty_object() {
        let policy: KiroRsToolPlusCachePolicy = serde_json::from_str("{}").unwrap();
        assert_eq!(policy, KiroRsToolPlusCachePolicy::default());
        assert!(!policy.creation_frequency.enabled);
        assert_eq!(policy.shaping.cache_read.multiplier, 1.0);
        assert!(policy.validate("p").is_ok());
    }

    #[test]
    fn partial_field_shaping_keeps_other_defaults() {
        let policy: KiroRsToolPlusCachePolicy = serde_json::from_str(
            r#"{"shaping":{"cacheRead":{"multiplier":3.5,"maxTokens":200000}}}"#,
        )
        .unwrap();
        assert_eq!(policy.shaping.cache_read.multiplier, 3.5);
        assert_eq!(policy.shaping.cache_read.max_tokens, 200_000);
        assert_eq!(policy.shaping.input.multiplier, 1.0);
    }

    #[test]
    fn rejects_out_of_range_values() {
        let mut policy = KiroRsToolPlusCachePolicy::default();
        policy.shaping.cache_read.multiplier = 1_001.0;
        assert!(policy.validate("p").is_err());

        let mut policy = KiroRsToolPlusCachePolicy::default();
        policy.shaping.output.min_tokens = 10;
        policy.shaping.output.max_tokens = 5;
        assert!(policy.validate("p").is_err());

        let mut policy = KiroRsToolPlusCachePolicy::default();
        policy.non_stream.profile.min_cost_ratio = 101.0;
        assert!(policy.validate("p").is_err());
    }

    #[test]
    fn selects_profile_by_priority() {
        let mut policy = KiroRsToolPlusCachePolicy::default();
        policy.shaping.input.multiplier = 1.0;
        policy.non_stream.mode = UsageShapingProfileMode::Custom;
        policy.non_stream.profile.input.multiplier = 2.0;
        policy.small_request.enabled = true;
        policy.small_request.max_input_tokens = 100;
        policy.small_request.mode = UsageShapingProfileMode::Raw;

        assert_eq!(
            policy.select_shaping(true, 1_000),
            SelectedUsageShaping::Profile(policy.shaping)
        );
        match policy.select_shaping(false, 1_000) {
            SelectedUsageShaping::Profile(profile) => assert_eq!(profile.input.multiplier, 2.0),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(policy.select_shaping(false, 100), SelectedUsageShaping::Raw);
        assert_eq!(policy.select_shaping(true, 50), SelectedUsageShaping::Raw);
    }
}
