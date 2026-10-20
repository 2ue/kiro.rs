//! 上下限优先缓存策略（`bounded_high_cache`）的下游 usage 整形。
//!
//! 模拟、前缀追踪、reportedUsage、creationControl 与 `current_high_cache` 相同；区别在最终一步：
//! 各字段上下限是最终约束，先由配置算出范围（[`FinalUsageBounds`]），整形只在范围内进行，
//! 返回前再按范围收口，之后不再改写。超出范围的部分直接丢弃，不挪到其他字段。
//!
//! 优先级：上下限 > 写入频次控制 > 命中率 > 不亏损。
//! 1. 按目标命中率（含浮动）在总量内分配读取；放行写入时非读取部分记为写入（input 保持
//!    原值），不放行时超出 input 上限的部分记为读取，读取到上限后丢弃；
//! 2. 放行写入时把写入抬到 `min_creation_ratio`；
//! 3. 计费低于目标（原始计费 × `min_cost_ratio`，放行写入时加上累计差额）时，在范围内依次
//!    抬高写入、读取、input；
//! 4. 补读取保持命中率，最后按范围收口（小值抬到最小值或清零）。
//!
//! 本次在范围内补不上的差额由调用方记账（见
//! `PromptCacheCreationController::record_cost_outcome`），留到之后放行写入的请求上补。
//! `hitShaping.enabled` 为 false 时只按上限收口，不做命中率、小值和计费整形。

use crate::model::config::CacheHitShapingPolicy;

use super::cache::{CacheUsage, normalize_shaped_cache_usage};
use super::pricing::ModelPricing;

/// 上下限优先整形的请求级输入。
#[derive(Debug, Clone, Copy)]
pub(crate) struct BoundedShapingInput {
    /// 路由的 hitShaping 参数（未开启时只收口上限）。
    pub policy: CacheHitShapingPolicy,
    /// 请求带历史或本地前缀命中，说明存在可复用前缀。
    pub has_reusable_prefix: bool,
    /// creation control 是否放行本次写入；不放行时最终写入为 0。
    pub creation_allowed: bool,
    /// 原始（未整形）usage，作为计费下限的基准。
    pub raw: CacheUsage,
    /// 模型单价；None 时跳过计费补偿。
    pub pricing: Option<ModelPricing>,
    /// 下游 input 上限（来自 reportedUsage.input）；0 表示不限。
    pub input_max_tokens: i32,
    /// 下游 cache_read / cache_creation 最终上限（0 表示不限）。
    pub max_cache_read_tokens: i32,
    pub max_cache_creation_tokens: i32,
    /// creation control 允许的本次写入上限（本地链路为本次放行量，外部池为单次上限；
    /// 0 表示不限）。
    pub max_creation_event_tokens: i32,
    /// 同一控制维度此前累计、尚未补回的计费差额（美元），只在放行写入的请求上补。
    pub carried_cost_deficit_usd: f64,
    pub seed: u64,
}

impl BoundedShapingInput {
    fn shaping_enabled(&self) -> bool {
        self.policy.normalized().enabled
    }

    /// 计费下限（原始计费 × `min_cost_ratio`）；未开启整形或没有单价时为 None。
    pub(crate) fn cost_floor_usd(&self) -> Option<f64> {
        let policy = self.policy.normalized();
        if !policy.enabled || policy.min_cost_ratio <= 0.0 {
            return None;
        }
        self.pricing
            .map(|pricing| pricing.estimate(self.raw) * policy.min_cost_ratio)
    }
}

/// 在最终下游 usage 上执行命中率、小值整形和计费补偿，结果每个字段都在范围内。
pub(crate) fn shape_bounded_usage(usage: CacheUsage, input: BoundedShapingInput) -> CacheUsage {
    let bounds = FinalUsageBounds::new(&input);
    let usage = normalize_shaped_cache_usage(usage);
    if !input.shaping_enabled() {
        return bounds.enforce(usage);
    }
    let policy = input.policy.normalized();
    let total = usage
        .input_tokens
        .max(0)
        .saturating_add(usage.cache_read_input_tokens.max(0))
        .saturating_add(usage.cache_creation_input_tokens.max(0));
    if total <= 1 {
        return bounds.enforce(usage);
    }

    let reusable = input.has_reusable_prefix || usage.cache_read_input_tokens > 0;
    let jitter_unit = unit(input.seed ^ 0x3c6e_f372_fe94_f82b);
    let target = (policy.target_hit_ratio + policy.ratio_jitter * jitter_unit)
        .min(CacheHitShapingPolicy::MAX_TARGET_HIT_RATIO);
    let shape_ratio = reusable && policy.target_hit_ratio > 0.0;

    let mut shaped = if shape_ratio {
        split_by_ratio(&usage, total, target, &bounds, input.creation_allowed)
    } else {
        usage
    };
    if !input.creation_allowed {
        // creation control 未放行：本次不上报写入，也不把写入并回 input。
        shaped.cache_creation_input_tokens = 0;
    }
    raise_creation_to_min(&mut shaped, &input, policy, &bounds);
    if let (Some(pricing), Some(floor)) = (input.pricing, input.cost_floor_usd()) {
        let mut target_cost = floor;
        if input.creation_allowed && input.carried_cost_deficit_usd.is_finite() {
            target_cost += input.carried_cost_deficit_usd.max(0.0);
        }
        raise_cost_within_bounds(&mut shaped, pricing, target_cost, &bounds, shape_ratio);
    }
    if shape_ratio {
        raise_read_to_ratio(&mut shaped, target, bounds.read_max);
    }
    bounds.enforce(shaped)
}

/// 只按范围收口，不做其他整形。用于整形之后还有其他约束（如冷请求约束）的场景。
pub(crate) fn enforce_bounded_usage(usage: CacheUsage, input: &BoundedShapingInput) -> CacheUsage {
    FinalUsageBounds::new(input).enforce(usage)
}

/// 最终下游 usage 每个字段的允许范围，由配置和本次请求状态算出。
///
/// 上限在配置值以下做确定性浮动（input 15%，读写 5%），触及上限的请求不会集中在同一个
/// 整数上。最小值只约束非零值：低于最小值时抬到最小值，最小值超出上限时清零。
#[derive(Debug, Clone, Copy)]
struct FinalUsageBounds {
    input_max: i32,
    read_min: i32,
    read_max: i32,
    creation_min: i32,
    creation_max: i32,
}

const INPUT_CAP_JITTER_PERCENT: i64 = 15;
const CACHE_CAP_JITTER_PERCENT: i64 = 5;

impl FinalUsageBounds {
    fn new(input: &BoundedShapingInput) -> Self {
        let cap = |value: i32| if value > 0 { value } else { i32::MAX };
        let policy = input.policy.normalized();
        let mut read_max = cap(input.max_cache_read_tokens);
        if policy.enabled && policy.max_read_multiplier > 0.0 {
            let raw_input = raw_total(&input.raw);
            let multiplier_cap = ((raw_input.max(1) as f64) * policy.max_read_multiplier)
                .round()
                .clamp(1.0, i32::MAX as f64) as i32;
            read_max = read_max.min(multiplier_cap);
        }
        let creation_max = if input.creation_allowed {
            cap(input.max_cache_creation_tokens).min(cap(input.max_creation_event_tokens))
        } else {
            0
        };
        let (read_min, creation_min) = if policy.enabled {
            (
                policy.min_cache_read_tokens.max(0),
                policy.min_cache_creation_tokens.max(0),
            )
        } else {
            (0, 0)
        };
        Self {
            input_max: jittered_cap(
                cap(input.input_max_tokens),
                INPUT_CAP_JITTER_PERCENT,
                input.seed ^ 0x2545_f491_4f6c_dd1d,
            ),
            read_min,
            read_max: jittered_cap(
                read_max,
                CACHE_CAP_JITTER_PERCENT,
                input.seed ^ 0x9e6c_63d0_676a_9a99,
            ),
            creation_min,
            creation_max: jittered_cap(
                creation_max,
                CACHE_CAP_JITTER_PERCENT,
                input.seed ^ 0x7c8f_1e2b_5a3d_9f01,
            ),
        }
    }

    fn clamp_input(&self, value: i32) -> i32 {
        value.clamp(1, self.input_max.max(1))
    }

    /// 非零缓存字段收口：低于最小值抬到最小值，超过上限截到上限；上限低于最小值时清零。
    fn clamp_cache_field(value: i32, min: i32, max: i32) -> i32 {
        let value = value.max(0);
        if value == 0 || max <= 0 || max < min {
            return 0;
        }
        value.clamp(min, max)
    }

    fn enforce(&self, mut usage: CacheUsage) -> CacheUsage {
        let has_input = usage.input_tokens > 0
            || usage.cache_read_input_tokens > 0
            || usage.cache_creation_input_tokens > 0;
        usage.input_tokens = if has_input {
            self.clamp_input(usage.input_tokens.max(0))
        } else {
            0
        };
        usage.cache_read_input_tokens =
            Self::clamp_cache_field(usage.cache_read_input_tokens, self.read_min, self.read_max);
        usage.cache_creation_input_tokens = Self::clamp_cache_field(
            usage.cache_creation_input_tokens,
            self.creation_min,
            self.creation_max,
        );
        normalize_shaped_cache_usage(usage)
    }
}

/// 在 `max` 以下做确定性浮动：结果落在 `[max - max × percent%, max]`。不限（`i32::MAX`）原样返回。
fn jittered_cap(max: i32, percent: i64, seed: u64) -> i32 {
    if max <= 1 || max == i32::MAX {
        return max;
    }
    let span = (i64::from(max) * percent / 100).min(i64::from(max) - 1);
    if span <= 0 {
        return max;
    }
    max - (splitmix64(seed) % (span as u64 + 1)) as i32
}

/// 放行写入时，把写入抬到原始输入 × `min_creation_ratio`（不超过写入上限）。
fn raise_creation_to_min(
    usage: &mut CacheUsage,
    input: &BoundedShapingInput,
    policy: CacheHitShapingPolicy,
    bounds: &FinalUsageBounds,
) {
    if !input.creation_allowed || policy.min_creation_ratio <= 0.0 {
        return;
    }
    let raw_input = raw_total(&input.raw);
    let min_creation = ((raw_input as f64) * policy.min_creation_ratio)
        .round()
        .clamp(0.0, i32::MAX as f64) as i32;
    let min_creation = min_creation
        .max(bounds.creation_min)
        .min(bounds.creation_max);
    if usage.cache_creation_input_tokens < min_creation {
        usage.cache_creation_input_tokens = min_creation;
    }
}

/// 在总量 `total` 内按读取占比 `ratio` 分配，所有字段都在范围内。
///
/// 放行写入时 input 保持原值（收口到范围内），其余非读取部分记为写入；不放行时非读取部分
/// 记为 input。超出 input / 写入上限的部分记为读取，读取到上限后丢弃。
fn split_by_ratio(
    usage: &CacheUsage,
    total: i32,
    ratio: f64,
    bounds: &FinalUsageBounds,
    allow_creation: bool,
) -> CacheUsage {
    let ratio = ratio.clamp(0.0, CacheHitShapingPolicy::MAX_TARGET_HIT_RATIO);
    let mut read = (((total as f64) * ratio).round() as i32)
        .clamp(0, total)
        .min(bounds.read_max);
    let non_read = total - read;
    let input_target = if allow_creation {
        usage.input_tokens
    } else {
        non_read
    };
    let input = bounds.clamp_input(input_target.min(non_read));
    let rest = non_read.saturating_sub(input).max(0);
    let creation = if allow_creation {
        rest.min(bounds.creation_max)
    } else {
        0
    };
    let overflow = rest - creation;
    read = read.saturating_add(overflow.min(bounds.read_max.saturating_sub(read).max(0)));
    CacheUsage {
        input_tokens: input,
        cache_read_input_tokens: read,
        cache_creation_input_tokens: creation,
        ..*usage
    }
}

/// 增加 read 使 read / (input + read + creation) >= ratio，不超过读取上限。
fn raise_read_to_ratio(usage: &mut CacheUsage, ratio: f64, read_max: i32) {
    let others = usage
        .input_tokens
        .max(0)
        .saturating_add(usage.cache_creation_input_tokens.max(0)) as f64;
    if others <= 0.0 || ratio <= 0.0 || read_max <= 0 {
        return;
    }
    let needed = ((ratio * others) / (1.0 - ratio))
        .ceil()
        .min(read_max as f64) as i32;
    if usage.cache_read_input_tokens < needed {
        usage.cache_read_input_tokens = needed;
    }
}

/// 计费低于 `target_cost` 时在范围内补足：依次抬高写入（已放行）、读取（有可复用前缀）、
/// input，每个字段都不超过自己的上限。范围内补不上时保留差额，由调用方记账。
fn raise_cost_within_bounds(
    usage: &mut CacheUsage,
    pricing: ModelPricing,
    target_cost: f64,
    bounds: &FinalUsageBounds,
    allow_read: bool,
) {
    let gap = |usage: &CacheUsage| target_cost - pricing.estimate(*usage);
    let tokens_for = |gap: f64, unit_cost: f64| -> i64 {
        if gap <= 0.0 || unit_cost <= 0.0 {
            0
        } else {
            (gap / unit_cost).ceil().clamp(0.0, i32::MAX as f64) as i64
        }
    };
    let raise = |current: i32, tokens: i64, min: i32, max: i32| -> i32 {
        if tokens <= 0 || max <= 0 {
            return current;
        }
        let raised = (i64::from(current.max(0)) + tokens).max(i64::from(min));
        raised.min(i64::from(max)).max(i64::from(current)) as i32
    };

    if bounds.creation_max > 0 && bounds.creation_min <= bounds.creation_max {
        let tokens = tokens_for(gap(usage), pricing.cache_creation_input_token_cost);
        usage.cache_creation_input_tokens = raise(
            usage.cache_creation_input_tokens,
            tokens,
            bounds.creation_min,
            bounds.creation_max,
        );
    }
    if allow_read && bounds.read_max > 0 && bounds.read_min <= bounds.read_max {
        let tokens = tokens_for(gap(usage), pricing.cache_read_input_token_cost);
        usage.cache_read_input_tokens = raise(
            usage.cache_read_input_tokens,
            tokens,
            bounds.read_min,
            bounds.read_max,
        );
    }
    let tokens = tokens_for(gap(usage), pricing.input_cost_per_token);
    usage.input_tokens = raise(usage.input_tokens, tokens, 1, bounds.input_max);
}

fn raw_total(raw: &CacheUsage) -> i32 {
    raw.input_tokens
        .max(0)
        .saturating_add(raw.cache_read_input_tokens.max(0))
        .saturating_add(raw.cache_creation_input_tokens.max(0))
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

    fn opus55() -> ModelPricing {
        ModelPricing {
            input_cost_per_token: 0.000004,
            output_cost_per_token: 0.00002,
            cache_creation_input_token_cost: 0.000005,
            cache_read_input_token_cost: 0.0000002,
        }
    }

    fn shaping(target: f64, min_cost_ratio: f64) -> CacheHitShapingPolicy {
        CacheHitShapingPolicy {
            enabled: true,
            target_hit_ratio: target,
            ratio_jitter: 0.04,
            min_cache_read_tokens: 1_024,
            min_cache_creation_tokens: 1_024,
            min_cost_ratio,
            max_read_multiplier: 0.0,
            min_creation_ratio: 0.0,
        }
    }

    fn input(policy: CacheHitShapingPolicy, raw: CacheUsage) -> BoundedShapingInput {
        BoundedShapingInput {
            policy,
            has_reusable_prefix: true,
            creation_allowed: true,
            raw,
            pricing: Some(opus55()),
            input_max_tokens: 20_000,
            max_cache_read_tokens: 0,
            max_cache_creation_tokens: 0,
            max_creation_event_tokens: 0,
            carried_cost_deficit_usd: 0.0,
            seed: 0,
        }
    }

    fn sum(usage: CacheUsage) -> i32 {
        usage.input_tokens + usage.cache_read_input_tokens + usage.cache_creation_input_tokens
    }

    #[test]
    fn minerst_input_cap_is_final_and_jittered() {
        // 现网 minerst 03:13:21：原始 39_012，input 上限 500，读取 5 倍上限，写入未放行。
        // current_high_cache 为了 0.3 倍计费下限把 input 补到 8_198；这里 input 不超过 500 且不贴边。
        let raw = raw(39_012, 126);
        let policy = CacheHitShapingPolicy {
            target_hit_ratio: 0.95,
            max_read_multiplier: 5.0,
            min_creation_ratio: 2.0,
            ..shaping(0.95, 0.3)
        };
        let mut inputs = std::collections::BTreeSet::new();
        for seed in 0..256 {
            let shaped = shape_bounded_usage(
                usage(31_000, 81_000, 0, 126),
                BoundedShapingInput {
                    creation_allowed: false,
                    input_max_tokens: 500,
                    seed,
                    ..input(policy, raw)
                },
            );
            assert!((1..=500).contains(&shaped.input_tokens), "{shaped:?}");
            assert_eq!(shaped.cache_creation_input_tokens, 0, "{shaped:?}");
            assert!(shaped.cache_read_input_tokens <= 39_012 * 5, "{shaped:?}");
            assert_eq!(shaped.total_input_tokens, sum(shaped));
            inputs.insert(shaped.input_tokens);
        }
        assert!(inputs.len() > 20, "input cap is jittered: {inputs:?}");
    }

    #[test]
    fn sweep_never_leaves_bounds_or_reports_small_values() {
        let mut state = 0x1234_u64;
        for _ in 0..5_000 {
            state = splitmix64(state);
            let raw_tokens = ((splitmix64(state ^ 3) % 150_000) + 2) as i32;
            let scale = 1.0 + (state % 300) as f64 / 100.0;
            let total = ((raw_tokens as f64) * scale) as i32;
            let read = (splitmix64(state ^ 1) % (total as u64 + 1)) as i32
                * ((!state.is_multiple_of(3)) as i32);
            let creation = (splitmix64(state ^ 2) % ((total - read) as u64 + 1)) as i32;
            let base = usage(total - read - creation, read, creation, 1);
            let creation_allowed = !state.is_multiple_of(5);
            let input_cap = 1 + (splitmix64(state ^ 4) % 20_000) as i32;
            let read_cap = (splitmix64(state ^ 5) % 600_000) as i32;
            let event_cap = (splitmix64(state ^ 6) % 60_000) as i32;
            let shaped = shape_bounded_usage(
                base,
                BoundedShapingInput {
                    has_reusable_prefix: state.is_multiple_of(2),
                    creation_allowed,
                    input_max_tokens: input_cap,
                    max_cache_read_tokens: read_cap,
                    max_creation_event_tokens: event_cap,
                    carried_cost_deficit_usd: (state % 7) as f64 * 0.01,
                    seed: state,
                    ..input(
                        CacheHitShapingPolicy {
                            max_read_multiplier: (state % 6) as f64,
                            min_creation_ratio: (state % 3) as f64,
                            ..shaping((state % 100) as f64 / 100.0, 1.0)
                        },
                        raw(raw_tokens, 1),
                    )
                },
            );
            let r = shaped.cache_read_input_tokens;
            let c = shaped.cache_creation_input_tokens;
            assert!(
                (1..=input_cap).contains(&shaped.input_tokens),
                "{base:?} -> {shaped:?}"
            );
            if read_cap > 0 {
                assert!(r <= read_cap, "{base:?} -> {shaped:?}");
            }
            let multiplier = (state % 6) as f64;
            if multiplier > 0.0 {
                assert!(r as f64 <= raw_tokens as f64 * multiplier + 1.0);
            }
            if !creation_allowed {
                assert_eq!(c, 0, "suppressed request wrote {c}: {base:?}");
            }
            if event_cap > 0 {
                assert!(c <= event_cap, "{base:?} -> {shaped:?}");
            }
            assert!(r == 0 || r >= 1_024, "read {r} from {base:?}");
            assert!(c == 0 || c >= 1_024, "creation {c} from {base:?}");
            assert_eq!(shaped.total_input_tokens, sum(shaped));
            assert_eq!(
                shaped.cache_creation_5m_input_tokens + shaped.cache_creation_1h_input_tokens,
                c
            );
        }
    }

    #[test]
    fn cost_floor_is_met_when_bounds_allow_it() {
        // 范围足够时仍然不亏损。
        let raw = raw(30_000, 10);
        let shaped = shape_bounded_usage(
            usage(3_000, 20_000, 7_000, 10),
            input(shaping(0.9, 1.0), raw),
        );
        assert!(opus55().estimate(shaped) >= opus55().estimate(raw) * (1.0 - 1e-9));
        assert!(shaped.input_tokens <= 20_000);
    }

    #[test]
    fn carried_deficit_is_repaid_only_when_creation_is_allowed() {
        let raw = raw(40_000, 100);
        let base = usage(400, 150_000, 20_000, 100);
        let shaping_input = |carried: f64, creation_allowed: bool| BoundedShapingInput {
            creation_allowed,
            input_max_tokens: 500,
            max_creation_event_tokens: 30_000,
            carried_cost_deficit_usd: carried,
            seed: 7,
            ..input(shaping(0.95, 0.3), raw)
        };
        let without = shape_bounded_usage(base, shaping_input(0.0, true));
        let with = shape_bounded_usage(base, shaping_input(0.05, true));
        assert!(
            opus55().estimate(with) > opus55().estimate(without),
            "{without:?} {with:?}"
        );
        assert!(with.cache_creation_input_tokens <= 30_000, "{with:?}");
        assert!(with.input_tokens <= 500, "{with:?}");
        assert_eq!(
            shape_bounded_usage(base, shaping_input(0.0, false)),
            shape_bounded_usage(base, shaping_input(0.05, false))
        );
    }

    #[test]
    fn disabled_shaping_only_enforces_caps() {
        let raw = raw(60_000, 10);
        let bounded = BoundedShapingInput {
            input_max_tokens: 500,
            max_creation_event_tokens: 30_000,
            ..input(CacheHitShapingPolicy::disabled(), raw)
        };
        let shaped = shape_bounded_usage(usage(60_000, 9, 50_000, 10), bounded);
        assert!(shaped.input_tokens <= 500, "{shaped:?}");
        assert!(shaped.cache_creation_input_tokens <= 30_000, "{shaped:?}");
        // 未开启整形时不抬小值。
        assert_eq!(shaped.cache_read_input_tokens, 9);
        assert_eq!(enforce_bounded_usage(shaped, &bounded), shaped);
    }

    #[test]
    fn creation_never_exceeds_controlled_allowance() {
        // creation control 因窗口余量只放行了 5_000：整形（命中率、最小写入比例、计费补偿）
        // 都不能把写入抬到放行量之上。
        let raw = raw(39_000, 300);
        let policy = CacheHitShapingPolicy {
            min_creation_ratio: 2.0,
            ..shaping(0.95, 1.0)
        };
        for seed in 0..64 {
            let shaped = shape_bounded_usage(
                usage(39_000, 0, 5_000, 300),
                BoundedShapingInput {
                    input_max_tokens: 500,
                    max_creation_event_tokens: 5_000,
                    carried_cost_deficit_usd: 1.0,
                    seed,
                    ..input(policy, raw)
                },
            );
            assert!(shaped.cache_creation_input_tokens <= 5_000, "{shaped:?}");
            assert!(shaped.input_tokens <= 500, "{shaped:?}");
        }
    }
}
