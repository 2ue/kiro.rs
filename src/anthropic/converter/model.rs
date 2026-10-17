//! Model mapping and Kiro native reasoning field selection.

use crate::anthropic::model_capabilities::strip_model_compat_suffixes;
use crate::anthropic::model_capabilities::{
    KiroReasoningCapabilityState, KiroReasoningFieldCapability, KiroReasoningFieldPath,
};
use crate::anthropic::types::{MessagesRequest, THINKING_EFFORT_VALUES, parse_thinking_effort};
use crate::kiro::model::requests::kiro::{
    AdditionalModelRequestFields, KiroOutputConfig, KiroReasoningConfig, KiroThinkingConfig,
};

/// Normalize a request-side Claude model without selecting a Kiro model version.
///
/// Version selection belongs to `ModelCapabilitiesCatalog`, which has the current
/// account-derived model directory and the configured mapping switches. This helper
/// is retained for legacy conversion call sites that do not have a catalog;
/// it must therefore never invent a concrete Sonnet/Opus/Haiku target.
pub fn map_model(model: &str) -> Option<String> {
    let (model_base, _) = strip_model_compat_suffixes(model);
    let model_base = model_base.trim().to_ascii_lowercase();
    if model_base.is_empty() {
        return None;
    }
    if matches!(
        model_base.as_str(),
        "auto" | "best" | "default" | "opus" | "opusplan" | "sonnet" | "haiku" | "fable"
    ) {
        return Some(model_base);
    }
    let rest = model_base.strip_prefix("claude-")?;
    let has_known_family = rest
        .split(['-', '.'])
        .any(|part| matches!(part, "opus" | "sonnet" | "haiku" | "fable"));
    has_known_family.then_some(model_base)
}

/// 根据模型名称返回对应的上下文窗口大小
///
/// 这是仅在 Kiro `ListAvailableModels` 能力目录缺失时使用的保守兜底。
/// 真实请求应优先使用上游目录中的 `maxInputTokens`；同名/同族模型在不同
/// 账号池中可能是 200K 或 1M，不能仅凭普通别名把 free Sonnet 误抬成 1M。
pub fn get_context_window_size(model: &str) -> i32 {
    let model_lower = model.to_lowercase();
    if model_lower.contains("[1m]") {
        return 1_000_000;
    }

    200_000
}

const EFFORTS_WITH_XHIGH: &[&str] = &["low", "medium", "high", "xhigh", "max"];
const EFFORTS_WITHOUT_XHIGH: &[&str] = &["low", "medium", "high", "max"];

fn legacy_native_reasoning_capability(model_id: &str) -> Option<KiroReasoningFieldCapability> {
    let (base, _) = strip_model_compat_suffixes(model_id);
    let mut parts = base.split('-');
    if parts.next() != Some("claude") {
        return None;
    }
    let family = parts.next()?;
    if !matches!(family, "opus" | "sonnet") {
        return None;
    }
    let version = parts.next()?;
    let (major, minor) = if let Some((major, minor)) = version.split_once('.') {
        (major.parse::<u32>().ok()?, minor.parse::<u32>().ok())
    } else {
        (
            version.parse::<u32>().ok()?,
            parts.next().and_then(|value| value.parse::<u32>().ok()),
        )
    };
    let (efforts, default_effort) = match (family, major, minor) {
        ("opus", 4, Some(minor)) if minor >= 7 => (EFFORTS_WITH_XHIGH, "xhigh"),
        ("opus", 4, Some(6)) | ("sonnet", 4, Some(6)) => (EFFORTS_WITHOUT_XHIGH, "high"),
        _ => return None,
    };
    Some(KiroReasoningFieldCapability {
        path: KiroReasoningFieldPath::OutputConfig,
        efforts: efforts.iter().map(|effort| (*effort).to_string()).collect(),
        default_effort: Some(default_effort.to_string()),
    })
}

fn has_explicit_output_effort(req: &MessagesRequest) -> bool {
    req.output_config
        .as_ref()
        .and_then(|output_config| output_config.effort.as_deref())
        .is_some()
}

fn thinking_is_disabled(req: &MessagesRequest) -> bool {
    req.thinking
        .as_ref()
        .is_some_and(|thinking| thinking.thinking_type == "disabled")
}

pub(super) fn requested_native_reasoning(req: &MessagesRequest) -> bool {
    if thinking_is_disabled(req) {
        return has_explicit_output_effort(req);
    }
    req.thinking.as_ref().is_some_and(|t| t.is_enabled()) || req.output_config.is_some()
}

fn effort_from_budget_tokens(tokens: i32) -> &'static str {
    match tokens {
        i32::MIN..=4_000 => "low",
        4_001..=16_000 => "medium",
        16_001..=64_000 => "high",
        _ => "xhigh",
    }
}

fn select_native_reasoning_effort(
    req: &MessagesRequest,
    capability: &KiroReasoningFieldCapability,
) -> Result<String, super::ConversionError> {
    if let Some(explicit_effort) = req
        .output_config
        .as_ref()
        .and_then(|output_config| output_config.effort.as_deref())
    {
        let Some(requested) = parse_thinking_effort(explicit_effort) else {
            return Err(super::ConversionError::UnsupportedContent(format!(
                "unsupported output_config.effort: {explicit_effort}"
            )));
        };
        return nearest_supported_effort(requested, &capability.efforts).ok_or_else(|| {
            super::ConversionError::UnsupportedContent(format!(
                "output_config.effort {requested} is not supported by the selected upstream model"
            ))
        });
    }

    let requested = match req.thinking.as_ref() {
        Some(thinking) if thinking.thinking_type == "enabled" => {
            effort_from_budget_tokens(thinking.budget_tokens)
        }
        _ => {
            return capability.default_effort.clone().ok_or_else(|| {
                let detail = if req.output_config.is_some() {
                    " for omitted output_config.effort"
                } else {
                    ""
                };
                super::ConversionError::UnsupportedContent(format!(
                    "the selected upstream reasoning schema has no default effort{detail}"
                ))
            });
        }
    };
    nearest_supported_effort(requested, &capability.efforts).ok_or_else(|| {
        super::ConversionError::UnsupportedContent(format!(
            "thinking budget maps to effort {requested}, which is not supported by the selected upstream model"
        ))
    })
}

/// Picks the supported upstream effort closest to `requested`; on a tie the stronger level
/// wins (so `xhigh` becomes `max` when only `high` and `max` exist). Claude Code sends
/// efforts such as `xhigh` that some upstream models do not list; serving the nearest level
/// keeps the request working. `None` only when the model lists no known effort.
fn nearest_supported_effort(requested: &str, supported: &[String]) -> Option<String> {
    if supported.iter().any(|effort| effort == requested) {
        return Some(requested.to_string());
    }
    let rank = |effort: &str| {
        THINKING_EFFORT_VALUES
            .iter()
            .position(|value| *value == effort)
    };
    let requested_rank = rank(requested)?;
    let chosen = supported
        .iter()
        .filter_map(|effort| rank(effort).map(|position| (position, effort)))
        .min_by_key(|(position, _)| (position.abs_diff(requested_rank), usize::MAX - position))
        .map(|(_, effort)| effort.clone())?;
    tracing::info!(
        requested_effort = requested,
        upstream_effort = %chosen,
        "mapped reasoning effort to the nearest level the upstream model supports"
    );
    Some(chosen)
}

pub(super) fn build_additional_model_request_fields(
    req: &MessagesRequest,
    model_id: &str,
    enabled: bool,
    capability_state: &KiroReasoningCapabilityState,
    force_visible_thinking: bool,
) -> Result<Option<AdditionalModelRequestFields>, super::ConversionError> {
    if !enabled {
        return Ok(None);
    }
    if thinking_is_disabled(req) && !has_explicit_output_effort(req) {
        return Ok(None);
    }

    if !requested_native_reasoning(req) {
        return Ok(None);
    }

    let capability = match capability_state {
        KiroReasoningCapabilityState::Supported(capability) => capability.clone(),
        KiroReasoningCapabilityState::LegacyFallback => {
            let Some(capability) = legacy_native_reasoning_capability(model_id) else {
                return Ok(None);
            };
            capability
        }
        KiroReasoningCapabilityState::Unknown
        | KiroReasoningCapabilityState::AuthoritativeAbsent
        | KiroReasoningCapabilityState::AuthoritativeInvalid => return Ok(None),
    };

    let effort = select_native_reasoning_effort(req, &capability)?;
    Ok(Some(match capability.path {
        KiroReasoningFieldPath::OutputConfig => AdditionalModelRequestFields {
            thinking: (!thinking_is_disabled(req)).then(|| KiroThinkingConfig {
                thinking_type: "adaptive".to_string(),
                display: force_visible_thinking.then(|| "summarized".to_string()),
            }),
            output_config: Some(KiroOutputConfig { effort }),
            reasoning: None,
        },
        KiroReasoningFieldPath::Reasoning => AdditionalModelRequestFields {
            thinking: None,
            output_config: None,
            reasoning: Some(KiroReasoningConfig { effort }),
        },
    }))
}

pub(super) fn uses_native_reasoning_fields(
    req: &MessagesRequest,
    model_id: &str,
    enabled: bool,
    capability_state: &KiroReasoningCapabilityState,
) -> bool {
    let disabled_without_explicit_effort =
        thinking_is_disabled(req) && !has_explicit_output_effort(req);
    enabled
        && !disabled_without_explicit_effort
        && requested_native_reasoning(req)
        && (capability_state.capability().is_some()
            || (matches!(
                capability_state,
                KiroReasoningCapabilityState::LegacyFallback
            ) && legacy_native_reasoning_capability(model_id).is_some()))
}

#[cfg(test)]
mod nearest_effort_tests {
    use super::nearest_supported_effort;

    fn levels(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn unsupported_efforts_map_to_the_nearest_supported_level() {
        let four = levels(&["low", "medium", "high", "max"]);
        assert_eq!(
            nearest_supported_effort("high", &four).as_deref(),
            Some("high")
        );
        // xhigh sits between high and max: the tie goes to the stronger level.
        assert_eq!(
            nearest_supported_effort("xhigh", &four).as_deref(),
            Some("max")
        );
        let three = levels(&["low", "medium", "high"]);
        assert_eq!(
            nearest_supported_effort("xhigh", &three).as_deref(),
            Some("high")
        );
        assert_eq!(
            nearest_supported_effort("max", &three).as_deref(),
            Some("high")
        );
        let upper = levels(&["high", "max"]);
        assert_eq!(
            nearest_supported_effort("low", &upper).as_deref(),
            Some("high")
        );
        assert_eq!(nearest_supported_effort("xhigh", &levels(&[])), None);
        assert_eq!(nearest_supported_effort("bogus", &three), None);
    }
}
