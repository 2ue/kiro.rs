//! 上游返回 200 但 usage 里输入或输出为 0 时的估算修复（路由缓存策略 `zeroUsageRepair`）。
//!
//! 只在上游 usage 进入本系统的入口改一次：把估算值写回上游 usage，之后的整形、采集都当作上游
//! 本来就给了这个值，所以照常按路由整形并受最终上下限约束；路由本次不整形（原样转发上游 usage）
//! 时，补出来的输出在这里按路由最终输出上限裁剪，输入与上游真实输入一样原样转发（输入字段的
//! sample-max 是把超出部分挪进缓存的整形规则，不是截断上限）。只对按路由策略整形 usage 的外部池生效。
//! 绝大多数请求 usage 正常，这里只做几次整数比较；请求输入要现数 token 时才解析请求体。

use super::*;
use crate::model::config::{
    ReportedUsagePathPolicy, ZeroUsageFallback, ZeroUsageRepairPolicy, ZeroUsageSidePolicy,
};

#[derive(Debug, Clone)]
pub(super) struct ZeroUsageRepairContext {
    policy: ZeroUsageRepairPolicy,
    /// 已经算好的请求输入估算；没有时按需从请求体现数。
    known_request_input_tokens: Option<i32>,
    request_body: Bytes,
    output_cap: Option<i32>,
}

/// 本次修了哪一侧；用于标记估算与写原因。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct ZeroUsageRepairOutcome {
    pub(super) input: bool,
    pub(super) output: bool,
}

impl ZeroUsageRepairOutcome {
    pub(super) fn any(self) -> bool {
        self.input || self.output
    }

    pub(super) fn reason(self) -> &'static str {
        match (self.input, self.output) {
            (true, true) => "zero_upstream_input_output",
            (true, false) => "zero_upstream_input",
            _ => "zero_upstream_output",
        }
    }
}

impl ZeroUsageRepairContext {
    /// 外部池透传上游 usage、或路由两侧都关闭时返回 `None`。
    pub(super) fn for_attempt(
        route: &ExternalRouteRequest,
        pool: &ExternalPool,
        projection: Option<&ExternalUsageProjectionContext>,
    ) -> Option<Self> {
        if pool.usage_projection_mode != ExternalPoolUsageProjectionMode::CurrentPathPolicy {
            return None;
        }
        let policy = route.zero_usage_repair;
        if !policy.is_active() {
            return None;
        }
        let reported = route.reported_usage.policy_for_path(&route.endpoint);
        Some(Self {
            policy,
            known_request_input_tokens: projection
                .map(|projection| projection.raw_input_tokens)
                .filter(|tokens| *tokens > 0)
                .or_else(|| (route.request_input_tokens > 0).then_some(route.request_input_tokens)),
            request_body: route.effective_raw_body.clone(),
            output_cap: final_output_cap(&reported),
        })
    }

    pub(super) fn with_known_request_input_tokens(mut self, tokens: Option<i32>) -> Self {
        if let Some(tokens) = tokens.filter(|tokens| *tokens > 0) {
            self.known_request_input_tokens = Some(tokens);
        }
        self
    }

    fn request_input_tokens(&self) -> i32 {
        self.known_request_input_tokens
            .unwrap_or_else(|| {
                serde_json::from_slice::<MessagesRequest>(&self.request_body)
                    .map(|payload| count_external_route_input_tokens(&payload))
                    .unwrap_or(0)
            })
            .max(0)
    }
}

/// 路由最终输出上限：先扣掉最大扣减量，保证不超过任何一次抖动后的有效上限。
fn final_output_cap(policy: &ReportedUsagePathPolicy) -> Option<i32> {
    (policy.enabled && policy.final_output_guard_enabled && policy.final_output_max_tokens > 0)
        .then(|| {
            let jitter = policy
                .final_output_jitter_min_tokens
                .max(policy.final_output_jitter_max_tokens)
                .max(0);
            policy.final_output_max_tokens.saturating_sub(jitter).max(1)
        })
}

/// 修一个 usage 对象：`prior` 是同一响应前面事件给出的用量（流式 message_start），
/// `output_estimate` 是按回复内容估算的输出。输入以输入、缓存读、缓存写之和判断是否为 0。
pub(super) fn repair_usage_value(
    usage: &mut serde_json::Value,
    prior: Option<CacheUsage>,
    output_estimate: i32,
    ctx: &ZeroUsageRepairContext,
) -> ZeroUsageRepairOutcome {
    let mut outcome = ZeroUsageRepairOutcome::default();
    if !usage.is_object() {
        return outcome;
    }
    let merged = match (prior, cache_usage_from_any_value(usage)) {
        (prior, Some(current)) => merge_external_usage(prior, current),
        (Some(prior), None) => prior,
        (None, None) => zero_cache_usage(),
    };
    let input_total = merged
        .input_tokens
        .max(0)
        .saturating_add(merged.cache_read_input_tokens.max(0))
        .saturating_add(merged.cache_creation_input_tokens.max(0));
    let output_estimate = output_estimate.max(0);
    let mut input_for_ratio = input_total;
    let Some(object) = usage.as_object_mut() else {
        return outcome;
    };
    if ctx.policy.input.enabled && input_total <= 0 {
        let value = resolve_side(
            ctx.request_input_tokens(),
            &ctx.policy.input,
            merged.output_tokens.max(output_estimate),
            None,
        );
        if value > 0 {
            set_usage_tokens(object, "input_tokens", "prompt_tokens", value);
            input_for_ratio = value;
            outcome.input = true;
        }
    }
    if ctx.policy.output.enabled && merged.output_tokens <= 0 {
        let value = resolve_side(
            output_estimate,
            &ctx.policy.output,
            input_for_ratio,
            ctx.output_cap,
        );
        if value > 0 {
            set_usage_tokens(object, "output_tokens", "completion_tokens", value);
            outcome.output = true;
        }
    }
    outcome
}

/// 流式：只修最终用量事件（顶层 `usage`，或 `delta.usage`），`message_start` 不动——它的输出
/// 本来就是 0 或 1，下游也会用 message_delta 里大于 0 的值覆盖它。不需要修时返回 `None`。
pub(super) fn repair_sse_event(
    event: &[u8],
    capture: &Arc<SyncMutex<ExternalUsageCapture>>,
) -> Option<Vec<u8>> {
    if !event.windows(7).any(|window| window == b"\"usage\"") {
        return None;
    }
    let (ctx, prior, output_estimate) = {
        let capture = capture.lock();
        (
            capture.zero_usage_repair.clone()?,
            capture.raw,
            capture.estimated_output_tokens,
        )
    };
    let text = std::str::from_utf8(event).ok()?;
    let mut outcome = ZeroUsageRepairOutcome::default();
    let mut out = String::with_capacity(text.len() + 32);
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches(['\r', '\n']);
        let ending = &line[body.len()..];
        let rewritten = body.strip_prefix("data:").and_then(|data| {
            let mut value = serde_json::from_str::<serde_json::Value>(data.trim()).ok()?;
            let usage = if value.get("usage").is_some_and(serde_json::Value::is_object) {
                value.get_mut("usage")?
            } else {
                value.get_mut("delta")?.get_mut("usage")?
            };
            let repaired = repair_usage_value(usage, prior, output_estimate, &ctx);
            if !repaired.any() {
                return None;
            }
            outcome.input |= repaired.input;
            outcome.output |= repaired.output;
            serde_json::to_string(&value).ok()
        });
        match rewritten {
            Some(data) => {
                out.push_str("data: ");
                out.push_str(&data);
                out.push_str(ending);
            }
            None => out.push_str(line),
        }
    }
    if !outcome.any() {
        return None;
    }
    mark_estimated(&mut capture.lock(), outcome);
    Some(out.into_bytes())
}

/// 非流式上游 usage 全为 0 时已按“缺失用量”估算写回响应体；估算结果里仍为 0 的一侧
/// （回复内容为空等）再按兜底规则补值，并同步到采集的原始与上报用量。
pub(super) fn apply_fallback_to_estimated_body(
    value: &mut serde_json::Value,
    capture: &mut ExternalUsageCapture,
    ctx: &ZeroUsageRepairContext,
) {
    let Some(usage) = value.get_mut("usage") else {
        return;
    };
    let outcome = repair_usage_value(usage, None, 0, ctx);
    if !outcome.any() {
        return;
    }
    let repaired = cache_usage_from_any_value(usage);
    capture.reported = repaired.or(capture.reported);
    if let (Some(raw), Some(repaired)) = (capture.raw.as_mut(), repaired) {
        if outcome.input {
            raw.input_tokens = repaired.input_tokens;
            raw.total_input_tokens = repaired.total_input_tokens;
        }
        if outcome.output {
            raw.output_tokens = repaired.output_tokens;
        }
    }
    if outcome.output {
        capture.estimated_output_tokens = capture
            .estimated_output_tokens
            .max(repaired.map_or(0, |usage| usage.output_tokens));
    }
}

pub(super) fn mark_estimated(capture: &mut ExternalUsageCapture, outcome: ZeroUsageRepairOutcome) {
    capture.usage_estimated = true;
    capture.usage_estimate_reason = Some(outcome.reason().to_string());
}

fn resolve_side(estimate: i32, side: &ZeroUsageSidePolicy, other: i32, cap: Option<i32>) -> i32 {
    let min = side.min_tokens.max(1);
    let max = side.max_tokens.max(min);
    let value = if estimate > 0 {
        estimate
    } else {
        match side.fallback {
            ZeroUsageFallback::Keep => 0,
            ZeroUsageFallback::Range => fastrand::i32(min..=max),
            ZeroUsageFallback::Ratio => {
                let scaled = (f64::from(other.max(0)) * side.ratio_percent / 100.0).round();
                (scaled.min(f64::from(i32::MAX)) as i32).clamp(min, max)
            }
        }
    };
    cap.map_or(value, |cap| value.min(cap))
}

/// 写 Anthropic 字段；上游用 OpenAI 字段名时同步写，避免规范化时取回旧值。
fn set_usage_tokens(
    object: &mut serde_json::Map<String, serde_json::Value>,
    anthropic_key: &str,
    openai_key: &str,
    value: i32,
) {
    object.insert(anthropic_key.to_string(), serde_json::Value::from(value));
    if object.contains_key(openai_key) {
        object.insert(openai_key.to_string(), serde_json::Value::from(value));
    }
}

fn zero_cache_usage() -> CacheUsage {
    CacheUsage {
        total_input_tokens: 0,
        input_tokens: 0,
        output_tokens: 0,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        cache_creation_5m_input_tokens: 0,
        cache_creation_1h_input_tokens: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx(policy: ZeroUsageRepairPolicy, known_input: Option<i32>) -> ZeroUsageRepairContext {
        ZeroUsageRepairContext {
            policy,
            known_request_input_tokens: known_input,
            request_body: Bytes::new(),
            output_cap: None,
        }
    }

    fn side(fallback: ZeroUsageFallback, min: i32, max: i32, ratio: f64) -> ZeroUsageSidePolicy {
        ZeroUsageSidePolicy {
            enabled: true,
            fallback,
            min_tokens: min,
            max_tokens: max,
            ratio_percent: ratio,
        }
    }

    #[test]
    fn zero_input_and_output_use_request_and_reply_estimates() {
        let mut usage = json!({"input_tokens": 0, "output_tokens": 0});
        let outcome = repair_usage_value(
            &mut usage,
            None,
            37,
            &ctx(ZeroUsageRepairPolicy::default(), Some(1_200)),
        );
        assert_eq!(
            outcome,
            ZeroUsageRepairOutcome {
                input: true,
                output: true
            }
        );
        assert_eq!(outcome.reason(), "zero_upstream_input_output");
        assert_eq!(usage["input_tokens"], 1_200);
        assert_eq!(usage["output_tokens"], 37);
    }

    #[test]
    fn normal_usage_and_cache_only_input_are_left_untouched() {
        let policy = ZeroUsageRepairPolicy::default();
        let mut normal = json!({"input_tokens": 5, "output_tokens": 9});
        assert!(!repair_usage_value(&mut normal, None, 50, &ctx(policy, Some(100))).any());
        assert_eq!(normal, json!({"input_tokens": 5, "output_tokens": 9}));
        // input_tokens 为 0 但缓存读不为 0：输入不算 0。
        let mut cached =
            json!({"input_tokens": 0, "cache_read_input_tokens": 800, "output_tokens": 3});
        assert!(!repair_usage_value(&mut cached, None, 50, &ctx(policy, Some(100))).any());
        assert_eq!(cached["input_tokens"], 0);
    }

    #[test]
    fn disabled_side_keeps_upstream_zero() {
        let mut policy = ZeroUsageRepairPolicy::default();
        policy.input.enabled = false;
        let mut usage = json!({"input_tokens": 0, "output_tokens": 0});
        let outcome = repair_usage_value(&mut usage, None, 12, &ctx(policy, Some(900)));
        assert_eq!(
            outcome,
            ZeroUsageRepairOutcome {
                input: false,
                output: true
            }
        );
        assert_eq!(outcome.reason(), "zero_upstream_output");
        assert_eq!(usage["input_tokens"], 0);
        assert_eq!(usage["output_tokens"], 12);
    }

    #[test]
    fn zero_estimate_with_keep_fallback_changes_nothing() {
        let mut usage = json!({"input_tokens": 300, "output_tokens": 0});
        let outcome = repair_usage_value(
            &mut usage,
            None,
            0,
            &ctx(ZeroUsageRepairPolicy::default(), None),
        );
        assert!(!outcome.any());
        assert_eq!(usage["output_tokens"], 0);
    }

    #[test]
    fn zero_estimate_with_range_fallback_stays_inside_range() {
        let policy = ZeroUsageRepairPolicy {
            output: side(ZeroUsageFallback::Range, 3, 9, 0.5),
            ..ZeroUsageRepairPolicy::default()
        };
        for _ in 0..200 {
            let mut usage = json!({"input_tokens": 300, "output_tokens": 0});
            assert!(repair_usage_value(&mut usage, None, 0, &ctx(policy, None)).output);
            let value = usage["output_tokens"].as_i64().unwrap();
            assert!((3..=9).contains(&value), "{value}");
        }
    }

    #[test]
    fn zero_estimate_with_ratio_fallback_scales_other_side_within_range() {
        let policy = ZeroUsageRepairPolicy {
            output: side(ZeroUsageFallback::Ratio, 1, 100, 0.5),
            ..ZeroUsageRepairPolicy::default()
        };
        let mut usage = json!({"input_tokens": 10_000, "output_tokens": 0});
        assert!(repair_usage_value(&mut usage, None, 0, &ctx(policy, None)).output);
        assert_eq!(usage["output_tokens"], 50);
        // 比例结果超出范围时收在范围内。
        let mut large = json!({"input_tokens": 1_000_000, "output_tokens": 0});
        repair_usage_value(&mut large, None, 0, &ctx(policy, None));
        assert_eq!(large["output_tokens"], 100);
        let mut small = json!({"input_tokens": 10, "output_tokens": 0});
        repair_usage_value(&mut small, None, 0, &ctx(policy, None));
        assert_eq!(small["output_tokens"], 1);
    }

    #[test]
    fn repaired_values_respect_route_final_caps() {
        let mut capped = ctx(ZeroUsageRepairPolicy::default(), Some(50_000));
        capped.output_cap = Some(64);
        let mut usage = json!({"input_tokens": 0, "output_tokens": 0});
        repair_usage_value(&mut usage, None, 9_000, &capped);
        // 输出按最终输出上限裁剪；输入与上游真实输入一样不截断。
        assert_eq!(usage["input_tokens"], 50_000);
        assert_eq!(usage["output_tokens"], 64);
        // 兜底生成的值同样受上限约束。
        let mut fallback = ctx(
            ZeroUsageRepairPolicy {
                output: side(ZeroUsageFallback::Range, 100, 200, 0.5),
                ..ZeroUsageRepairPolicy::default()
            },
            None,
        );
        fallback.output_cap = Some(64);
        let mut usage = json!({"input_tokens": 10, "output_tokens": 0});
        repair_usage_value(&mut usage, None, 0, &fallback);
        assert_eq!(usage["output_tokens"], 64);

        let policy = ReportedUsagePathPolicy {
            final_output_guard_enabled: true,
            final_output_max_tokens: 1_000,
            final_output_jitter_min_tokens: 20,
            final_output_jitter_max_tokens: 50,
            ..ReportedUsagePathPolicy::default()
        };
        assert_eq!(final_output_cap(&policy), Some(950));
        assert_eq!(
            final_output_cap(&ReportedUsagePathPolicy {
                enabled: false,
                ..policy.clone()
            }),
            None
        );
    }

    #[test]
    fn openai_style_usage_gets_both_field_names() {
        let mut usage = json!({"prompt_tokens": 0, "completion_tokens": 0});
        repair_usage_value(
            &mut usage,
            None,
            8,
            &ctx(ZeroUsageRepairPolicy::default(), Some(640)),
        );
        assert_eq!(usage["prompt_tokens"], 640);
        assert_eq!(usage["input_tokens"], 640);
        assert_eq!(usage["completion_tokens"], 8);
        assert_eq!(usage["output_tokens"], 8);
    }

    #[test]
    fn prior_message_start_input_counts_toward_the_zero_check() {
        let prior = CacheUsage {
            input_tokens: 2_000,
            total_input_tokens: 2_000,
            output_tokens: 1,
            ..zero_cache_usage()
        };
        let mut usage = json!({"output_tokens": 0});
        let outcome = repair_usage_value(
            &mut usage,
            Some(prior),
            40,
            &ctx(ZeroUsageRepairPolicy::default(), Some(9)),
        );
        // message_start 已给出输入、输出 1：两侧都不算 0。
        assert!(!outcome.any());

        let prior_zero = CacheUsage {
            output_tokens: 0,
            ..zero_cache_usage()
        };
        let mut usage = json!({"output_tokens": 0});
        let outcome = repair_usage_value(
            &mut usage,
            Some(prior_zero),
            40,
            &ctx(ZeroUsageRepairPolicy::default(), Some(900)),
        );
        assert_eq!(
            outcome,
            ZeroUsageRepairOutcome {
                input: true,
                output: true
            }
        );
        assert_eq!(usage, json!({"output_tokens": 40, "input_tokens": 900}));
    }

    #[test]
    fn unknown_request_input_is_counted_from_the_request_body_on_demand() {
        let mut lazy = ctx(ZeroUsageRepairPolicy::default(), None);
        lazy.request_body = Bytes::from_static(
            br#"{"model":"claude-sonnet-4-6","max_tokens":16,"messages":[{"role":"user","content":"Summarize the following paragraph about distributed systems in two sentences."}]}"#,
        );
        assert!(lazy.request_input_tokens() > 0);
        let known = lazy.clone().with_known_request_input_tokens(Some(77));
        assert_eq!(known.request_input_tokens(), 77);
        assert_eq!(
            lazy.clone()
                .with_known_request_input_tokens(Some(0))
                .known_request_input_tokens,
            None
        );
    }

    #[test]
    fn policy_validation_rejects_invalid_ranges() {
        let valid = ZeroUsageRepairPolicy::default();
        assert!(valid.validate("p").is_ok());
        let mut bad = valid;
        bad.output.min_tokens = 0;
        assert!(bad.validate("p").unwrap_err().contains("minTokens"));
        let mut bad = valid;
        bad.input.max_tokens = 0;
        assert!(bad.validate("p").unwrap_err().contains("maxTokens"));
        let mut bad = valid;
        bad.output.ratio_percent = 0.0;
        assert!(bad.validate("p").unwrap_err().contains("ratioPercent"));
    }
}
