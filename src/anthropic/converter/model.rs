//! Model mapping and Kiro native reasoning field selection.

use crate::anthropic::model_capabilities::strip_model_1m_suffix;
use crate::anthropic::types::{MessagesRequest, normalize_thinking_effort};
use crate::kiro::model::requests::kiro::{
    AdditionalModelRequestFields, KiroOutputConfig, KiroReasoningConfig,
};

/// 模型映射：将 Anthropic 模型名映射到 Kiro 模型 ID
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
/// 这是仅在 Kiro `ListAvailableModels` 能力目录缺失时使用的保守兜底。
/// 真实请求应优先使用上游目录中的 `maxInputTokens`；同名/同族模型在不同
/// 账号池中可能是 200K 或 1M，不能仅凭普通别名把 free Sonnet 误抬成 1M。
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeReasoningSchemaPath {
    OutputConfig,
    #[allow(dead_code)]
    Reasoning,
}

#[derive(Debug, Clone, Copy)]
struct NativeReasoningSchema {
    path: NativeReasoningSchemaPath,
    efforts: &'static [&'static str],
}

const EFFORTS_WITH_XHIGH: &[&str] = &["low", "medium", "high", "xhigh", "max"];
const EFFORTS_WITHOUT_XHIGH: &[&str] = &["low", "medium", "high", "max"];

fn native_reasoning_schema(model_id: &str) -> Option<NativeReasoningSchema> {
    match model_id {
        "claude-opus-4.8" | "claude-opus-4-8" | "claude-opus-4.7" | "claude-opus-4-7" => {
            Some(NativeReasoningSchema {
                path: NativeReasoningSchemaPath::OutputConfig,
                efforts: EFFORTS_WITH_XHIGH,
            })
        }
        "claude-opus-4.6" | "claude-opus-4-6" | "claude-sonnet-4.6" | "claude-sonnet-4-6" => {
            Some(NativeReasoningSchema {
                path: NativeReasoningSchemaPath::OutputConfig,
                efforts: EFFORTS_WITHOUT_XHIGH,
            })
        }
        _ => None,
    }
}

fn requested_native_reasoning(req: &MessagesRequest) -> bool {
    req.thinking.as_ref().is_some_and(|t| t.is_enabled())
        || req
            .output_config
            .as_ref()
            .is_some_and(|oc| !oc.effort.trim().is_empty())
}

fn effort_from_budget_tokens(tokens: i32) -> &'static str {
    match tokens {
        i32::MIN..=4_000 => "low",
        4_001..=16_000 => "medium",
        16_001..=64_000 => "high",
        _ => "xhigh",
    }
}

fn select_native_reasoning_effort(req: &MessagesRequest, schema: NativeReasoningSchema) -> String {
    let requested = req
        .output_config
        .as_ref()
        .map(|oc| normalize_thinking_effort(&oc.effort))
        .or_else(|| {
            req.thinking.as_ref().map(|t| {
                if t.thinking_type == "enabled" {
                    effort_from_budget_tokens(t.budget_tokens)
                } else {
                    normalize_thinking_effort("")
                }
            })
        })
        .unwrap_or_else(|| normalize_thinking_effort(""));

    if schema.efforts.contains(&requested) {
        requested.to_string()
    } else {
        schema.efforts.last().copied().unwrap_or("high").to_string()
    }
}

pub(super) fn build_additional_model_request_fields(
    req: &MessagesRequest,
    model_id: &str,
    enabled: bool,
) -> Option<AdditionalModelRequestFields> {
    if !enabled {
        return None;
    }
    if req
        .thinking
        .as_ref()
        .is_some_and(|t| t.thinking_type == "disabled")
    {
        return None;
    }

    let schema = native_reasoning_schema(model_id)?;
    if !requested_native_reasoning(req) {
        return None;
    }

    let effort = select_native_reasoning_effort(req, schema);
    Some(match schema.path {
        NativeReasoningSchemaPath::OutputConfig => AdditionalModelRequestFields {
            thinking: None,
            output_config: Some(KiroOutputConfig { effort }),
            reasoning: None,
        },
        NativeReasoningSchemaPath::Reasoning => AdditionalModelRequestFields {
            thinking: None,
            output_config: None,
            reasoning: Some(KiroReasoningConfig { effort }),
        },
    })
}

pub(super) fn uses_native_reasoning_fields(
    req: &MessagesRequest,
    model_id: &str,
    enabled: bool,
) -> bool {
    build_additional_model_request_fields(req, model_id, enabled).is_some()
}
