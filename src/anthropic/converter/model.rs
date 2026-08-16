//! Model mapping and local-upstream native reasoning field selection.

use crate::anthropic::model_capabilities::strip_model_1m_suffix;
use crate::anthropic::model_capabilities::{
    UpstreamReasoningCapabilityState, UpstreamReasoningFieldCapability, UpstreamReasoningFieldPath,
};
use crate::anthropic::types::{MessagesRequest, parse_thinking_effort};
use crate::local_upstream::request::{
    LocalUpstreamAdditionalModelRequestFields as AdditionalModelRequestFields,
    LocalUpstreamOutputConfig, LocalUpstreamReasoningConfig, LocalUpstreamThinkingConfig,
};

/// 模型映射：将 Anthropic 模型名映射到上游模型 ID
/// 严格对照版本号
pub fn map_model(model: &str) -> Option<String> {
    let model_lower = model.to_lowercase();
    let model_base = model_lower.strip_suffix("[1m]").unwrap_or(&model_lower);
    let model_base = model_base.strip_suffix("-thinking").unwrap_or(model_base);

    if matches!(model_base, "opus" | "opusplan" | "best" | "default") {
        Some("claude-opus-4.7".to_string())
    } else if model_base == "sonnet" {
        Some("claude-sonnet-4.6".to_string())
    } else if model_base == "haiku" {
        Some("claude-haiku-4.5".to_string())
    } else if is_native_claude_family_model(model_base, "sonnet") {
        if model_base.contains("4-6") || model_base.contains("4.6") {
            Some("claude-sonnet-4.6".to_string())
        } else if model_base.contains("4-5") || model_base.contains("4.5") {
            Some("claude-sonnet-4.5".to_string())
        } else {
            Some(model_base.to_string())
        }
    } else if model_base.contains("sonnet") {
        if model_base.contains("4-6") || model_base.contains("4.6") {
            Some("claude-sonnet-4.6".to_string())
        } else if model_base.contains("4-5") || model_base.contains("4.5") {
            Some("claude-sonnet-4.5".to_string())
        } else if model_base.contains("4")
            || model_base.contains("3-5")
            || model_base.contains("3.5")
        {
            Some("claude-sonnet-4.5".to_string())
        } else {
            None
        }
    } else if is_native_claude_family_model(model_base, "opus") {
        if model_base.contains("4-5") || model_base.contains("4.5") {
            Some("claude-opus-4.5".to_string())
        } else if model_base.contains("4-6") || model_base.contains("4.6") {
            Some("claude-opus-4.6".to_string())
        } else if model_base.contains("4-7") || model_base.contains("4.7") {
            Some("claude-opus-4.7".to_string())
        } else {
            Some(model_base.to_string())
        }
    } else if model_base.contains("opus") {
        if model_base.contains("4-5") || model_base.contains("4.5") {
            Some("claude-opus-4.5".to_string())
        } else if model_base.contains("4-6") || model_base.contains("4.6") {
            Some("claude-opus-4.6".to_string())
        } else if model_base.contains("4-7") || model_base.contains("4.7") {
            Some("claude-opus-4.7".to_string())
        } else if model_base.contains("4") {
            Some("claude-opus-4.7".to_string())
        } else {
            None
        }
    } else if is_native_claude_family_model(model_base, "haiku") {
        if model_base.contains("4-5") || model_base.contains("4.5") {
            Some("claude-haiku-4.5".to_string())
        } else {
            Some(model_base.to_string())
        }
    } else if model_base.contains("haiku") {
        Some("claude-haiku-4.5".to_string())
    } else {
        None
    }
}

fn is_native_claude_family_model(model: &str, family: &str) -> bool {
    model
        .strip_prefix("claude-")
        .and_then(|rest| rest.strip_prefix(family))
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(['-', '.']))
}

/// 根据模型名称返回对应的上下文窗口大小
///
/// 这是仅在上游 `ListAvailableModels` 能力目录缺失时使用的保守兜底。
/// 真实请求应优先使用上游目录中的 `maxInputTokens`；同名/同族模型在不同
/// 账号池中可能是 200K 或 1M，不能仅凭普通别名把标准 Sonnet 误抬成 1M。
pub fn get_context_window_size(model: &str) -> i32 {
    let model_lower = model.to_lowercase();
    let explicit_one_m = model_lower.ends_with("[1m]");
    let base = strip_model_1m_suffix(&model_lower);

    if base == "auto" {
        return 1_000_000;
    }

    if explicit_one_m
        || base == "claude-opus-4.8"
        || base == "claude-opus-4.8-thinking"
        || base == "claude-opus-4.7"
        || base == "claude-opus-4.7-thinking"
        || base == "claude-opus-4.6"
        || base == "claude-opus-4.6-thinking"
        || base == "claude-sonnet-4.6"
        || base == "claude-sonnet-4.6-thinking"
    {
        return 1_000_000;
    }

    200_000
}

const EFFORTS_WITH_XHIGH: &[&str] = &["low", "medium", "high", "xhigh", "max"];
const EFFORTS_WITHOUT_XHIGH: &[&str] = &["low", "medium", "high", "max"];

fn legacy_native_reasoning_capability(model_id: &str) -> Option<UpstreamReasoningFieldCapability> {
    let (efforts, default_effort) = match model_id {
        "claude-opus-4.8" | "claude-opus-4-8" | "claude-opus-4.7" | "claude-opus-4-7" => {
            (EFFORTS_WITH_XHIGH, "xhigh")
        }
        "claude-opus-4.6" | "claude-opus-4-6" | "claude-sonnet-4.6" | "claude-sonnet-4-6" => {
            (EFFORTS_WITHOUT_XHIGH, "high")
        }
        _ => return None,
    };
    Some(UpstreamReasoningFieldCapability {
        path: UpstreamReasoningFieldPath::OutputConfig,
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
    capability: &UpstreamReasoningFieldCapability,
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
        if !capability.efforts.iter().any(|effort| effort == requested) {
            return Err(super::ConversionError::UnsupportedContent(format!(
                "output_config.effort {requested} is not supported by the selected upstream model"
            )));
        }
        return Ok(requested.to_string());
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
    if capability.efforts.iter().any(|effort| effort == requested) {
        return Ok(requested.to_string());
    }
    if requested == "xhigh" && capability.efforts.iter().any(|effort| effort == "max") {
        return Ok("max".to_string());
    }
    Err(super::ConversionError::UnsupportedContent(format!(
        "thinking budget maps to effort {requested}, which is not supported by the selected upstream model"
    )))
}

pub(super) fn build_additional_model_request_fields(
    req: &MessagesRequest,
    model_id: &str,
    enabled: bool,
    capability_state: &UpstreamReasoningCapabilityState,
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
        UpstreamReasoningCapabilityState::Supported(capability) => capability.clone(),
        UpstreamReasoningCapabilityState::LegacyFallback => {
            let Some(capability) = legacy_native_reasoning_capability(model_id) else {
                return Ok(None);
            };
            capability
        }
        UpstreamReasoningCapabilityState::Unknown
        | UpstreamReasoningCapabilityState::AuthoritativeAbsent
        | UpstreamReasoningCapabilityState::AuthoritativeInvalid => return Ok(None),
    };

    let effort = select_native_reasoning_effort(req, &capability)?;
    Ok(Some(match capability.path {
        UpstreamReasoningFieldPath::OutputConfig => AdditionalModelRequestFields {
            thinking: (!thinking_is_disabled(req)).then(|| LocalUpstreamThinkingConfig {
                thinking_type: "adaptive".to_string(),
                display: force_visible_thinking.then(|| "summarized".to_string()),
            }),
            output_config: Some(LocalUpstreamOutputConfig { effort }),
            reasoning: None,
        },
        UpstreamReasoningFieldPath::Reasoning => AdditionalModelRequestFields {
            thinking: None,
            output_config: None,
            reasoning: Some(LocalUpstreamReasoningConfig { effort }),
        },
    }))
}

pub(super) fn uses_native_reasoning_fields(
    req: &MessagesRequest,
    model_id: &str,
    enabled: bool,
    capability_state: &UpstreamReasoningCapabilityState,
) -> bool {
    let disabled_without_explicit_effort =
        thinking_is_disabled(req) && !has_explicit_output_effort(req);
    enabled
        && !disabled_without_explicit_effort
        && requested_native_reasoning(req)
        && (capability_state.capability().is_some()
            || (matches!(
                capability_state,
                UpstreamReasoningCapabilityState::LegacyFallback
            ) && legacy_native_reasoning_capability(model_id).is_some()))
}
